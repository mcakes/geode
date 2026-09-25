# Line Pricer Part 3 (Tile) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Put the line pricer on screen. `geode-pricer` gains its tile — `PricerFactory`, `PricerContent`, `PricerTile`, a `DataTable` delegate over a prepared `GridModel`, header and footer, entry mode (`o` + shorthand), insert mode (cell editor and choice typeahead), the normal-mode verbs and `:` vocabulary, repricing through `Delivery::Price`, the refresh timer, the session record, write-behind to an in-memory `SheetStore`, and the bundled-theme sweep. `geode-app` registers it, pushes `BUILTIN_VIEWS` into the builtin config layer and refreshes it on reload.

**Architecture:** Everything that decides lives in `geode_pricer::core` as pure functions with plain `#[test]`s (cell parsing, insert placement, the undo stack, the tree expansion, the `:` vocabulary). The tile is thin: it owns one `Sheet`, routes every mutation through `Sheet::apply` via one `apply_edit` door that records into a strictly LIFO `UndoStack`, rebuilds an `Rc<GridModel>` (every cell a `SharedString` plus a `CellState`) on edit, delivery, expansion, view and clock changes — never per frame — and installs it through `install_model` → `TableState::refresh`. Pricing is one `PriceParams` per submission through `DataHandle::price`, answered by `Delivery::Price` and landed with `Sheet::deliver_all`. Sheets persist through the `SheetStore` seam; in this part the only implementation is `MemorySheetStore` (process lifetime), which Part 4 replaces with the DuckDB-backed store.

**Tech Stack:** Rust 2024, gpui + gpui-component (workspace-pinned), `gpui_component::table::{DataTable, TableDelegate, TableState}`, `geode_shell::{module, keymap, actions, choice::ChoiceList, vimfind, vimnav, shell::{chip, colours, control, listrow, scale}, clock::AppClock, fonts}`, `geode_data::DataHandle`, `geode_core::{pricing, nudge, clock, document, colour}`, `chrono 0.4.42`, `toml 1.1.4`, `tracing` (workspace), criterion `0.8.2`.

**Spec:** `docs/superpowers/specs/2026-09-19-geode-line-pricer-design.md` — §7.3–§7.4 (write-behind, names; the parts this plan brings forward), §8 (the tile, this part's subject), §9 (repricing), §10 (errors), §12 (the tile test list), §14 part 3, and §16–§17 (as built, Parts 1 and 2: the vocabulary is `geode_core::pricing`, the core API is what `geode_pricer::core` exports). Read §8, §9 and §17 before starting. The merged core is at `crates/geode-pricer/src/core/`; its README lists the rules it pins.

## Decisions made in planning

Each resolves a place where §7–§9 as written cannot be implemented literally against the merged code, or where the spec left a choice open. Task 14 records them in the spec's as-built section; the ones marked **(flag)** go to Matthew in the handoff.

1. **`nudge_text` moves to `geode_core::nudge`.** The spec (§8.4) says the pricer's editor nudges "through `nudge_text`", which lives in `geode-marketdata`; a module may not depend on a sibling (CLAUDE.md). It is pure, depends only on `geode_core::schema::ColumnType`, and has no harness entry, so it moves whole (tests included) and `geode_marketdata::core` re-exports it so every existing caller is untouched.
2. **A nudge steps by the TEXT's own precision, not the column's painted precision.** Strikes paint at two places (`LEVEL`), so "the painted precision" would step `5000` to `5000.01`; the market-data attribute rule (`nudge_text(.., None, ..)`: the digits the text carries) steps `5000` by `1` and `95%` by `1%`. A trailing `%` is stripped before the nudge and put back after. An empty shift nudges from `0`.
3. **`Sheet::mark_all_stale` is added to the core** as the third `pub` state mutator beside `deliver`/`deliver_all`: the refresh tick (§9.4) and `:price` must stale every line WITHOUT a revision bump (a tick is not an edit, and a result already in flight at the current revision must still install). It touches `state` only, then folds.
4. **Every submission carries every stale line; the in-flight map only decides WHETHER to submit.** §9.1 submits "every line that is Stale and not already in flight", and §9.2 drops "an outcome tagged older than the latest submission ... whole". Together they lose lines: batch 1 is in flight, an edit submits batch 2 without batch 1's lines, batch 1's outcome arrives with an older tag and is dropped, and batch 1's lines stay in flight forever. So: submit iff some stale line is not in flight at its current revision; the batch is ALL stale lines; the in-flight map is replaced by that batch's `(id → revision)`. The newer tag supersedes the older batch completely, which is what the worker's own latest-wins-per-key rule already assumes.
5. **A refused submission (`DataHandle::price` → `false`) arms a one-shot retry after 1 s** (`RETRY_AFTER`) and paints a header notice. §5.3 says "resubmits on the next frame, as every other request does", but a pricer tile follows no frame counter, so with the refresh timer off nothing else would ever resubmit.
6. **The tile self-arrives at a flip barrier.** Every visible tile's key is in a barrier (`Frame::open_flip`); the pricer submits no view query, so without the diagnostics tile's self-arrival (`geode-diagnostics/src/tile.rs:190-199`) every scope keystroke would hold every other tile for the 250 ms deadline.
7. **`SheetStore` is synchronous-with-a-pending-answer:** `load(name) -> Loaded { Rows(DocumentRows) | Missing | Pending }`, `save(name, rows) -> bool`, `contains(name) -> bool`. §7.1's `load(name, key, tag) -> bool` answered by a `Delivery::Query` is Part 4's production shape; this one lets Part 3 build the tile's whole load path (`loading` notice, install, reprice) against a fake, and Part 4's `Delivery::Query` arm calls the tile's `loaded(..)` — the method a `Pending` answer waits on — with the decoded rows. `list`/`remove` are Part 4's (they serve `:e` completions and `:rm`).
8. **Write-behind, restore-by-name and the open-name set move into Part 3** (§14 puts them in Part 4) because the seam exists here and they are tile logic: the idle timer, `to_rows` on fire, the refused-save notice, restoring `sheet` from the session record through `load`, `untitled-N` naming, and the factory's open-name set. **Part 4 keeps** the production store, the `pricer_sheets` declaration, `:e`/`:name`/`:new`/`:rm` and the `Delivery::Query` load arm. With `MemorySheetStore` a sheet survives closing and reopening a tile in one process, not a restart: a restored name with no document opens empty with a notice, exactly §7.4's "document is gone" path. **(flag)**
9. **`:e`, `:name`, `:new`, `:rm` parse and refuse** with "`:<verb>` is not built yet" (the command-locality rule: a known word refuses with its reason rather than "unknown command").
10. **The cursor is `(LineId, column index)`**, not a row index, so a delivery, an edit elsewhere or an expansion never moves it; the grid row is looked up on each sync. A removed cursor line falls back to the grid row at the old index, clamped.
11. **The entry placeholder is a grid row, never a sheet row.** `GridModel::build` takes `entry: Option<Place>` and paints an `Entry` row where that `Place` would land; the sheet changes only on a successful `enter`. `escape` "removes the placeholder" by dropping the `Option`.
12. **Insert placement (`o`/`shift+o`, `p`/`shift+p`) is pure** (`core::entry::place_for`, `core::clip::put_place`). `o` on a package row → its first leg; `shift+o` on a package row → before the package (a root); on a leg → the adjacent leg slot; on a root line → the adjacent root boundary. `p` of a PACKAGE always lands at a root boundary (before or after the cursor's whole root block); `p` of a line follows `o`'s rule.
13. **Packages open when created in this session, and the session record's `expanded` list is authoritative on restore.** A package typed with `o` or made with `g p` is added to the expansion so its legs show; `z R`/`z M` open and close all.
14. **Paint is resolved at render from a per-theme memo (`Paints`), not stored in the `GridModel`.** §8.2 says `GridModel::build(.., theme inputs)`; keeping the model theme-free means a theme switch rebuilds nothing but the memo, the model is testable without a window, and the bench measures the build alone. Every text colour is floored to `READABLE_RATIO` against the ground it paints on (the table ground for line rows, `secondary` over it for package rows), so the §8.2 sweep passes on every bundled theme with no exception list.
15. **Result cells are not sign-coloured in this part.** `cell_text` returns text only (the `GREEK`/`PRICE` formats carry `Colour::Sign`, which `format_number` answers and `cell_text` drops); §8.2's paint list names none. A follow-up if the desk wants it. **(flag)**
16. **`y` alone is not bound.** §8.5 lists `y`, `y y`, `y c`, but the matcher dispatches an exact match immediately (`docs/current/keymaps.md`, "Sequences and counts"), so binding `y` would make `y y` and `y c` unreachable. The pricer binds `y y` (row shorthand) and `y c` (column). The market-data fragment binds all three; that is out of scope here and reported in the handoff. **(flag)**
17. **The underlying typeahead offers the sheet's own underlyings and accepts free text.** §8.4's "plus the catalog's known underlyings when the frame has one" needs a catalog source for pricing underlyings that does not exist yet; deferred. An unmatched query commits the typed text (upper-cased). `type` and `barrier_type` accept only their vocabulary.
18. **A package row is read-only in every column** (§6.5 paints its instrument and shift columns blank; its quantity is structural). `i` on it says `read-only`.
19. **Delivery log levels:** `FutureRevision` and `NotALine` are bugs and log at `warn` with the ids (§10.1); `UnknownLine` is the ordinary result of deleting a line mid-round-trip and logs at `debug`; `OldRevision` is silent (an edit landed mid-flight, and the line is resubmitted).
20. **Reload reaches the factory through the frame's `config` counter, not `ShellEvent::ConfigReloaded`**, which only fires for five named docs (`hot_reload.rs:181-185`). `bridge::attach` observes `Frame::versions().config` (the diagnostics factory's pattern in `main.rs:365-420`) and calls `PricerFactory::reload(views, refresh, stale_after, cx)`; the factory holds weak handles to its tiles and tells each one.
21. **`[pricing] refresh` gets its reader here:** `bridge::pricing_refresh_from_config` — absent → 30 s, `"off"` → none, a duration → that, anything else → 30 s plus a warning diagnostic at `app.pricing.refresh`.
22. **Menu rows are the pricer's own**, re-implemented over `shell::listrow` (the market-data popup cannot be imported): Price all, Group, Ungroup, Undo, Redo, Delete row, then one row per loaded view (checked = current). "Name" is Part 4's.
23. **Deleting a line whose request is in flight is safe**: its id is gone from the sheet, `deliver_all` answers `UnknownLine`, and the tile logs it at `debug` (decision 19). Undo of that delete restores its last result and state (`Restore`), so it is resubmitted only if it was stale.

## Global Constraints

- `geode-pricer` depends on `geode-core`, `geode-shell`, `geode-data` (path, for `DataHandle` alone), `gpui`, `gpui-component`, `chrono = "0.4.42"`, `toml = "1.1.4"` and `tracing` (workspace). Dev-dependencies: `geode-core`, `geode-shell` and `geode-data` with `features = ["test-support"]`, `gpui` with `features = ["test-support"]`, `criterion = "0.8.2"`. **Never** `geode-blotter`, `geode-marketdata`, `geode-timeseries`, `geode-diagnostics` or `geode-pricing` — the mock is reached only through the data tier (PHILOSOPHY §1, "In-process calculation").
- `[lib] bench = false` stays; the bench target stays `[[bench]] name = "core" harness = false`.
- Every mutation of the tile's `Sheet` goes through `PricerTile::apply_edit` (or `apply_edits`), which calls `Sheet::apply` and records the `Undo`. The only other writes are `Sheet::deliver_all` (a delivery), `Sheet::mark_all_stale` (a tick or `:price`), and the `pub` fields `name`/`view`/`refresh` (`:view`, `:refresh` — not request changes, not undoable). The undo stack is strictly LIFO: nothing edits the sheet behind it.
- The grid model is built on edit, delivery, expansion, view, clock and entry changes — never in `render`. `render` compares (staleness), never formats.
- The app performs no financial arithmetic (PHILOSOPHY §1): a package summing its legs is aggregation and happens in `Sheet::fold_packages` alone.
- Both text inputs (the entry field and the cell editor, plus the choice typeahead's field) follow the market-data rules verbatim: `holds_focus` answers from focus handles; a closer blurs only when its OWN field is focused, then drops it; a click anywhere cancels (never commits) an open editor; no chord is bound in `insert` or `entry` mode (`ctrl+k` must keep opening the palette from inside a field).
- `:` commands change only this tile (never the frame, the shell, the config or the log levels); the sweep test pins it.
- `Delivery` matches are exhaustive with no wildcard arm.
- Keymap fragment predicates are plain conjunctions whose first identifier is `pricer`; every bound action is registered; every key spells (the `build_keymap` check in the factory test).
- Displayed times use `geode_core::clock::Clock` read off `AppClock` with `try_global` (never `chrono::Local`).
- Theme tokens and `scale::design` own presentation; no literal colours. Column widths are the vocabulary's pixel widths (`ColumnDef::default_width`, the known gap recorded in `core::columns`).
- `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace` and `cargo check -p geode-shell --features test-support --all-targets` pass at the end of every task. Skip `cargo bench --workspace --no-run` locally (CI runs it; `clippy --all-targets` type-checks the bench). Run `cargo test` for the crate you touched during a task and the workspace once before its commit. Commit at the end of every task.
- `zsh scripts/mutation-check.sh --anchors-only` before the final merge (Task 14); targeted names during development (`zsh scripts/mutation-check.sh "pricer tile"`).
- Do not touch the blotter, the market-data panel (beyond Task 1's re-export), the timeseries module or `geode-data`. The shell changes nothing in this part.
- This plan is written against `main` at `9d22fed`. Execute in a worktree (`superpowers:using-git-worktrees`); the working tree on `main` has an untracked `TODO.md` that is Matthew's and must not be committed.

---

## File map

| File | Responsibility |
|---|---|
| `crates/geode-core/src/nudge.rs` (moved from `geode-marketdata/src/core/nudge.rs`) | `nudge_text` |
| `crates/geode-core/src/lib.rs` | `pub mod nudge;` |
| `crates/geode-marketdata/src/core/mod.rs` | re-export `geode_core::nudge::nudge_text` |
| `crates/geode-pricer/src/core/sheet.rs` | `Sheet::mark_all_stale` |
| `crates/geode-pricer/src/core/shorthand.rs` | `parse_barrier_kind` becomes `pub(crate)` |
| `crates/geode-pricer/src/core/cell.rs` (new) | what the cell editor opens with, commits and nudges |
| `crates/geode-pricer/src/core/entry.rs` (new) | `place_for`, `next_place`, `history` |
| `crates/geode-pricer/src/core/clip.rs` (new) | `spec_of`, `put_place` |
| `crates/geode-pricer/src/core/tree.rs` (new) | `Expansion`, `visible_rows` |
| `crates/geode-pricer/src/core/undo.rs` (new) | `UndoStack` |
| `crates/geode-pricer/src/core/commands.rs` (new) | `Command`, `parse`, `completions` |
| `crates/geode-pricer/src/store.rs` (new) | `SheetStore`, `Loaded`, `MemorySheetStore` |
| `crates/geode-pricer/src/grid.rs` (new) | `GridModel`, `GridRow`, `GridCell`, `GridColumn` |
| `crates/geode-pricer/src/paint.rs` (new) | `Paints` (per-theme floored text colours) |
| `crates/geode-pricer/src/delegate.rs` (new) | `SheetDelegate: TableDelegate`, `ChevronClicked` |
| `crates/geode-pricer/src/header.rs` (new) | `HeaderModel`, `FooterModel`, their render fns |
| `crates/geode-pricer/src/popup.rs` (new) | the action menu and the choice typeahead |
| `crates/geode-pricer/src/content.rs` (new) | `ACTIONS`, `DEFAULT_KEYMAP`, `PricerContent`, `PricerFactory`, `PricerSettings` |
| `crates/geode-pricer/src/tile.rs` (new) | `PricerTile` |
| `crates/geode-pricer/src/lib.rs` | module list, `init` |
| `crates/geode-pricer/Cargo.toml`, `README.md` | deps; layout and rules |
| `crates/geode-pricer/benches/core.rs` | `grid_build_1000` |
| `Cargo.toml` (workspace) | `geode-pricer` in `[workspace.dependencies]` |
| `crates/geode-app/Cargo.toml`, `src/main.rs`, `src/bridge.rs` | registration, builtin views, reload, `[pricing] refresh` |
| `docs/current/features.md`, `docs/current/performance.md`, `docs/perf.md`, the spec, `scripts/mutation-check.sh` | Task 14 |

---
### Task 1: Shared prerequisites — `geode_core::nudge`, `Sheet::mark_all_stale`, `parse_barrier_kind`

**Files:**
- Move: `crates/geode-marketdata/src/core/nudge.rs` → `crates/geode-core/src/nudge.rs` (`git mv`)
- Modify: `crates/geode-core/src/lib.rs` (module list), `crates/geode-marketdata/src/core/mod.rs:15,26`
- Modify: `crates/geode-pricer/src/core/sheet.rs` (after `deliver_all`, and `mod tests`)
- Modify: `crates/geode-pricer/src/core/shorthand.rs:127` (`fn parse_barrier_kind` → `pub(crate) fn`)

**Interfaces:**
- Produces: `geode_core::nudge::nudge_text(text: &str, ty: ColumnType, precision: Option<usize>, steps: i64) -> Result<String, String>` (unchanged signature); `geode_marketdata::core::nudge_text` still resolves (re-export).
- Produces: `impl Sheet { pub fn mark_all_stale(&mut self) }`.
- Produces: `pub(crate) fn crate::core::shorthand::parse_barrier_kind(token: &str) -> Option<BarrierKind>`.

- [ ] **Step 1: Move `nudge.rs` into `geode-core`**

```bash
git mv crates/geode-marketdata/src/core/nudge.rs crates/geode-core/src/nudge.rs
```

In `crates/geode-core/src/lib.rs` add `pub mod nudge;` between `pub mod log;` and `pub mod panic;` (the list is alphabetical). Extend the crate doc's first line so it still names what the crate holds: `…series, pricing, and editor nudging.`

In `crates/geode-core/src/nudge.rs`:
- change `use geode_core::schema::ColumnType;` to `use crate::schema::ColumnType;`;
- replace the module doc (it names `crate::core::DateTimeField`, which does not exist in `geode-core` and would be a broken intra-doc link) with:

```rust
//! Arrow-key nudging of the text in an open numeric editor: one unit of a
//! precision per step. Pure — text in, text out — so a module decides
//! which type and precision its open field carries and writes the answer
//! back into its input. Nothing here commits. Shared by the market-data
//! panel and the line pricer, which may not depend on each other.
```

In `crates/geode-marketdata/src/core/mod.rs` delete `pub mod nudge;` (line 15) and change `pub use nudge::nudge_text;` (line 26) to:

```rust
/// Moved to `geode-core` so the line pricer shares it (line-pricer Part 3,
/// planning decision 1); re-exported so this crate's callers are unchanged.
pub use geode_core::nudge::nudge_text;
```

- [ ] **Step 2: Build both crates and run the moved tests**

Run: `cargo test -p geode-core nudge && cargo test -p geode-marketdata`
Expected: the five moved tests (`a_cell_steps_one_unit_of_its_precision`, `an_attribute_steps_at_the_precision_its_text_paints`, `a_date_is_not_nudged_as_text`, `float_rounding_never_leaks_into_the_text`, `unparseable_text_is_refused_naming_the_text`) pass under `geode-core`; `geode-marketdata` compiles and passes unchanged. If `geode-marketdata` fails to resolve `crate::core::nudge::…` anywhere, `grep -rn 'core::nudge' crates/geode-marketdata` and point it at `crate::core::nudge_text`.

- [ ] **Step 3: Write the failing `mark_all_stale` test**

Append to `mod tests` in `crates/geode-pricer/src/core/sheet.rs`:

```rust
    /// The refresh tick and `:price` (spec §9.4, §8.6; Part 3 planning
    /// decision 3): every LINE goes `Stale` — a failed one too, since a
    /// refusal may be transient — at its CURRENT revision, so a result
    /// already in flight still installs; packages fold to `Stale`.
    #[test]
    fn mark_all_stale_stales_every_line_and_bumps_no_revision() {
        let mut s = Sheet::new("t");
        push(&mut s, vec![line(spx(5000.0, OptionKind::Call), 1)]);
        push(&mut s, vec![callspread(1)]);
        let lines: Vec<usize> = (0..s.len()).filter(|r| s.is_line(*r)).collect();
        let answers: Vec<_> = lines
            .iter()
            .map(|r| (s.id(*r), s.revision(*r), Ok(result(10.0))))
            .collect();
        s.deliver_all(answers, at(0));
        // One line fails, so the sweep is seen to cover `Failed` too.
        let first = s.id(0);
        let rev = s.revision(0);
        s.apply(Edit::SetQty { row: 0, qty: 2 }).unwrap();
        assert_eq!(s.revision(0), rev, "SetQty changes no request");
        s.deliver(first, rev, Err("refused".into()), at(1));
        let before: Vec<u64> = (0..s.len()).map(|r| s.revision(r)).collect();

        s.mark_all_stale();

        for r in lines {
            assert_eq!(s.state(r), &LineState::Stale, "row {r}");
        }
        assert_eq!(s.state(1), &LineState::Stale, "the package folds to Stale");
        let after: Vec<u64> = (0..s.len()).map(|r| s.revision(r)).collect();
        assert_eq!(before, after, "a tick is not an edit");
        // A result at the unchanged revision still installs.
        let leg = 2;
        assert_eq!(
            s.deliver(s.id(leg), s.revision(leg), Ok(result(3.0)), at(2)),
            Delivered::Installed
        );
        assert_eq!(s.state(leg), &LineState::Fresh);
    }
```

- [ ] **Step 4: Run it to see it fail**

Run: `cargo test -p geode-pricer mark_all_stale`
Expected: FAIL to compile — "no method named `mark_all_stale`".

- [ ] **Step 5: Implement `mark_all_stale`**

In `crates/geode-pricer/src/core/sheet.rs`, directly after `deliver_all`:

```rust
    /// Every line `Stale` at its current revision, then one fold (spec
    /// §9.4's tick, §8.6's `:price`). The third state-only mutator beside
    /// `deliver`/`deliver_all`: a tick is not an edit, so no revision
    /// moves — a result already in flight at the current revision must
    /// still install when it lands.
    pub fn mark_all_stale(&mut self) {
        for row in 0..self.len() {
            if self.is_line(row) {
                self.state[row] = LineState::Stale;
            }
        }
        self.fold_packages();
    }
```

Update the module doc's second paragraph (lines 7–10) so it lists the new mutator: "the other `pub` mutators are [`Sheet::deliver`] and [`Sheet::deliver_all`] (a result landing, one or a batch), [`Sheet::mark_all_stale`] (a tick) and [`Sheet::fold_packages`] (a recompute)".

In `crates/geode-pricer/src/core/shorthand.rs:127` change `fn parse_barrier_kind` to `pub(crate) fn parse_barrier_kind` (Task 2's cell parser reads a barrier-type cell through it).

- [ ] **Step 6: Run the crate's tests**

Run: `cargo test -p geode-pricer`
Expected: PASS, including `mark_all_stale_stales_every_line_and_bumps_no_revision`.

- [ ] **Step 7: Workspace gate and commit**

Run: `cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`
Expected: all pass.

```bash
git add crates/geode-core/src/lib.rs crates/geode-core/src/nudge.rs crates/geode-marketdata/src/core/mod.rs crates/geode-pricer/src/core/sheet.rs crates/geode-pricer/src/core/shorthand.rs
git commit -m "refactor(core): share nudge_text; add Sheet::mark_all_stale for the pricer tick

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 2: `core::cell` — what the cell editor opens with, commits and nudges

**Files:**
- Create: `crates/geode-pricer/src/core/cell.rs`
- Modify: `crates/geode-pricer/src/core/mod.rs` (module list and re-exports)
- Test: `crates/geode-pricer/src/core/cell.rs` (`mod tests`)

**Interfaces:**
- Consumes: `Sheet` getters; `crate::core::shorthand::{parse_expiry, parse_strike, parse_barrier_kind, render_expiry, render_strike, render_barrier_kind}`; `ColumnKind`; `Edit`; `geode_core::nudge::nudge_text`.
- Produces:

```rust
pub const READ_ONLY: &str = "read-only";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CellEditor {
    /// A text field opened on this text.
    Text(String),
    /// A typeahead over `options`, highlighted on `current`; `free` lets an
    /// unmatched query commit as typed (the underlying).
    Choice { options: Vec<String>, current: String, free: bool },
}

pub fn editor_for(sheet: &Sheet, row: usize, kind: ColumnKind) -> Result<CellEditor, &'static str>;
pub fn commit(sheet: &Sheet, row: usize, kind: ColumnKind, text: &str) -> Result<Edit, String>;
pub fn nudge(kind: ColumnKind, text: &str, steps: i64) -> Result<String, String>;
```

- [ ] **Step 1: Write the failing tests**

Create `crates/geode-pricer/src/core/cell.rs` with the tests first (the implementation in Step 3 goes above them):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::edit::Edit;
    use crate::core::sheet::tests::{callspread, line, push, spx};
    use crate::core::sheet::{OwnShifts, Sheet};
    use geode_core::pricing::{Barrier, BarrierKind, Expiry, Instrument, OptionKind, Strike};

    fn one_line() -> Sheet {
        let mut s = Sheet::new("t");
        push(&mut s, vec![line(spx(5000.0, OptionKind::Call), -5)]);
        s
    }

    fn barrier_line() -> Sheet {
        let mut s = Sheet::new("t");
        let Instrument::Vanilla(v) = spx(5000.0, OptionKind::Call) else {
            unreachable!()
        };
        push(
            &mut s,
            vec![line(
                Instrument::Barrier(Barrier {
                    vanilla: v,
                    level: 4200.0,
                    barrier: BarrierKind::DownOut,
                }),
                1,
            )],
        );
        s
    }

    #[test]
    fn a_text_cell_opens_on_the_grammar_spelling_of_its_value() {
        let s = one_line();
        assert_eq!(editor_for(&s, 0, ColumnKind::Qty), Ok(CellEditor::Text("-5".into())));
        assert_eq!(editor_for(&s, 0, ColumnKind::Expiry), Ok(CellEditor::Text("Z26".into())));
        assert_eq!(editor_for(&s, 0, ColumnKind::Strike), Ok(CellEditor::Text("5000".into())));
        // An inherited shift opens EMPTY: an empty commit means "inherit".
        assert_eq!(editor_for(&s, 0, ColumnKind::SpotShift), Ok(CellEditor::Text(String::new())));
        let b = barrier_line();
        assert_eq!(editor_for(&b, 0, ColumnKind::Barrier), Ok(CellEditor::Text("4200".into())));
    }

    #[test]
    fn choice_cells_offer_their_vocabulary_and_the_underlying_takes_free_text() {
        let mut s = one_line();
        push(&mut s, vec![line(spx(4000.0, OptionKind::Put), 1)]);
        assert_eq!(
            editor_for(&s, 0, ColumnKind::Type),
            Ok(CellEditor::Choice {
                options: vec!["C".into(), "P".into()],
                current: "C".into(),
                free: false
            })
        );
        assert_eq!(
            editor_for(&s, 0, ColumnKind::Underlying),
            Ok(CellEditor::Choice {
                options: vec!["SPX".into()],
                current: "SPX".into(),
                free: true
            }),
            "the sheet's own underlyings, deduplicated and sorted"
        );
        let b = barrier_line();
        assert_eq!(
            editor_for(&b, 0, ColumnKind::BarrierType),
            Ok(CellEditor::Choice {
                options: vec!["UI".into(), "UO".into(), "DI".into(), "DO".into()],
                current: "DO".into(),
                free: false
            })
        );
    }

    #[test]
    fn results_packages_and_barrier_columns_on_a_vanilla_are_read_only() {
        let mut s = one_line();
        push(&mut s, vec![callspread(1)]);
        for kind in [
            ColumnKind::Price,
            ColumnKind::Delta,
            ColumnKind::Gamma,
            ColumnKind::Vega,
            ColumnKind::Theta,
            ColumnKind::Rho,
            ColumnKind::PricedAt,
            ColumnKind::Status,
            ColumnKind::Barrier,
            ColumnKind::BarrierType,
        ] {
            assert_eq!(editor_for(&s, 0, kind), Err(READ_ONLY), "{kind:?}");
        }
        for kind in [ColumnKind::Qty, ColumnKind::Strike, ColumnKind::SpotShift] {
            assert_eq!(editor_for(&s, 1, kind), Err(READ_ONLY), "package {kind:?}");
        }
    }

    #[test]
    fn a_commit_becomes_the_one_edit_its_column_means() {
        let s = one_line();
        assert_eq!(commit(&s, 0, ColumnKind::Qty, " 10 "), Ok(Edit::SetQty { row: 0, qty: 10 }));
        let Ok(Edit::SetInstrument { row: 0, instrument }) =
            commit(&s, 0, ColumnKind::Strike, "95%")
        else {
            panic!("a strike commit is SetInstrument")
        };
        assert_eq!(instrument.strike(), Strike::Percent(95.0));
        let Ok(Edit::SetInstrument { instrument, .. }) = commit(&s, 0, ColumnKind::Expiry, "3m")
        else {
            panic!()
        };
        assert_eq!(instrument.expiry(), &Expiry::Tenor("3m".into()));
        let Ok(Edit::SetInstrument { instrument, .. }) = commit(&s, 0, ColumnKind::Type, "p")
        else {
            panic!()
        };
        assert_eq!(instrument.kind(), OptionKind::Put);
        let Ok(Edit::SetInstrument { instrument, .. }) =
            commit(&s, 0, ColumnKind::Underlying, "ndx")
        else {
            panic!()
        };
        assert_eq!(instrument.underlying(), "NDX");
    }

    #[test]
    fn an_empty_shift_commit_inherits_and_a_signed_number_is_owned() {
        let s = one_line();
        assert_eq!(
            commit(&s, 0, ColumnKind::SpotShift, "+2"),
            Ok(Edit::SetShift {
                row: 0,
                shift: OwnShifts { spot_pct: Some(2.0), vol_pts: None }
            })
        );
        assert_eq!(
            commit(&s, 0, ColumnKind::VolShift, "  "),
            Ok(Edit::SetShift { row: 0, shift: OwnShifts::default() }),
            "empty means inherit, not zero"
        );
    }

    #[test]
    fn a_bad_commit_is_refused_with_the_reason_and_names_the_text() {
        let s = one_line();
        assert_eq!(commit(&s, 0, ColumnKind::Qty, "0"), Err("quantity must not be zero".into()));
        assert_eq!(commit(&s, 0, ColumnKind::Qty, "x"), Err("quantity 'x' is not a whole number".into()));
        assert!(commit(&s, 0, ColumnKind::Strike, "-1").unwrap_err().contains("positive"));
        assert_eq!(commit(&s, 0, ColumnKind::Type, "X"), Err("type 'X': C or P".into()));
        assert_eq!(commit(&s, 0, ColumnKind::Underlying, "S P"), Err("underlying 'S P': one word".into()));
        assert_eq!(commit(&s, 0, ColumnKind::Price, "1"), Err(READ_ONLY.into()));
        let b = barrier_line();
        assert_eq!(
            commit(&b, 0, ColumnKind::BarrierType, "UP"),
            Err("barrier type 'UP': UI UO DI DO".into())
        );
        assert_eq!(
            commit(&b, 0, ColumnKind::Barrier, "abc"),
            Err("barrier 'abc' is not a number".into())
        );
    }

    #[test]
    fn a_nudge_steps_by_the_texts_own_precision_and_keeps_a_percent() {
        assert_eq!(nudge(ColumnKind::Strike, "5000", 1), Ok("5001".into()));
        assert_eq!(nudge(ColumnKind::Strike, "95%", -10), Ok("85%".into()));
        assert_eq!(nudge(ColumnKind::Strike, "4250.5", 1), Ok("4250.6".into()));
        assert_eq!(nudge(ColumnKind::Qty, "-5", 1), Ok("-4".into()));
        assert_eq!(nudge(ColumnKind::SpotShift, "", 1), Ok("1".into()), "empty nudges from 0");
        assert!(nudge(ColumnKind::Expiry, "Z26", 1).is_err());
    }
}
```

In `crates/geode-pricer/src/core/mod.rs` add `pub mod cell;` (alphabetical, before `columns`) and `pub use cell::{CellEditor, READ_ONLY};`.

- [ ] **Step 2: Run to see it fail**

Run: `cargo test -p geode-pricer core::cell`
Expected: FAIL to compile — `editor_for`, `commit`, `nudge`, `CellEditor`, `READ_ONLY` not found.

- [ ] **Step 3: Implement**

Put above `mod tests` in `crates/geode-pricer/src/core/cell.rs`:

```rust
//! The cell editor's pure half (line-pricer spec §8.4): what an editable
//! cell opens with, what a commit means as ONE `Edit`, and how an arrow
//! key nudges the open text. The tile only opens an `InputState` on the
//! answer and hands the committed text back here.

use crate::core::columns::ColumnKind;
use crate::core::edit::Edit;
use crate::core::sheet::{OwnShifts, Sheet};
use crate::core::shorthand::{
    parse_barrier_kind, parse_expiry, parse_strike, render_barrier_kind, render_expiry,
    render_strike,
};
use geode_core::nudge::nudge_text;
use geode_core::pricing::{Instrument, OptionKind, Vanilla};
use geode_core::schema::ColumnType;

/// The footer's word for a cell that does not edit (spec §8.4).
pub const READ_ONLY: &str = "read-only";

const TYPES: [&str; 2] = ["C", "P"];
const BARRIER_TYPES: [&str; 4] = ["UI", "UO", "DI", "DO"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CellEditor {
    /// A text field opened on this text.
    Text(String),
    /// A typeahead over `options`, highlighted on `current`; `free` lets an
    /// unmatched query commit as typed (the underlying, planning decision 17).
    Choice {
        options: Vec<String>,
        current: String,
        free: bool,
    },
}

/// The line's instrument, or `READ_ONLY` for a package (planning decision
/// 18) — every editable column reads one.
fn instrument(sheet: &Sheet, row: usize) -> Result<&Instrument, &'static str> {
    sheet.instrument(row).ok_or(READ_ONLY)
}

fn barrier(i: &Instrument) -> Result<(f64, geode_core::pricing::BarrierKind), &'static str> {
    match i {
        Instrument::Barrier(b) => Ok((b.level, b.barrier)),
        Instrument::Vanilla(_) => Err(READ_ONLY),
    }
}

/// `{}` of an `f64`: `2` for `2.0`, `4250.5` as typed — the editor opens on
/// the shortest spelling that parses back.
fn plain(v: f64) -> String {
    format!("{v}")
}

fn kind_token(kind: OptionKind) -> &'static str {
    match kind {
        OptionKind::Call => "C",
        OptionKind::Put => "P",
    }
}

/// What the editor opens with on (`row`, `kind`), or why it does not open.
pub fn editor_for(sheet: &Sheet, row: usize, kind: ColumnKind) -> Result<CellEditor, &'static str> {
    let i = instrument(sheet, row)?;
    Ok(match kind {
        ColumnKind::Qty => CellEditor::Text(sheet.qty(row).to_string()),
        ColumnKind::Expiry => CellEditor::Text(render_expiry(i.expiry())),
        ColumnKind::Strike => CellEditor::Text(render_strike(i.strike())),
        ColumnKind::Barrier => CellEditor::Text(plain(barrier(i)?.0)),
        ColumnKind::SpotShift => {
            CellEditor::Text(sheet.shift(row).spot_pct.map(plain).unwrap_or_default())
        }
        ColumnKind::VolShift => {
            CellEditor::Text(sheet.shift(row).vol_pts.map(plain).unwrap_or_default())
        }
        ColumnKind::Type => CellEditor::Choice {
            options: TYPES.iter().map(|s| s.to_string()).collect(),
            current: kind_token(i.kind()).to_string(),
            free: false,
        },
        ColumnKind::BarrierType => CellEditor::Choice {
            options: BARRIER_TYPES.iter().map(|s| s.to_string()).collect(),
            current: render_barrier_kind(barrier(i)?.1).to_string(),
            free: false,
        },
        ColumnKind::Underlying => {
            let mut options: Vec<String> = (0..sheet.len())
                .filter_map(|r| sheet.instrument(r).map(|i| i.underlying().to_string()))
                .collect();
            options.sort();
            options.dedup();
            CellEditor::Choice {
                options,
                current: i.underlying().to_string(),
                free: true,
            }
        }
        ColumnKind::Price
        | ColumnKind::Delta
        | ColumnKind::Gamma
        | ColumnKind::Vega
        | ColumnKind::Theta
        | ColumnKind::Rho
        | ColumnKind::PricedAt
        | ColumnKind::Status => return Err(READ_ONLY),
    })
}

/// The instrument with its vanilla part changed — through a barrier too.
fn with_vanilla(i: &Instrument, f: impl FnOnce(&mut Vanilla)) -> Instrument {
    let mut out = i.clone();
    match &mut out {
        Instrument::Vanilla(v) => f(v),
        Instrument::Barrier(b) => f(&mut b.vanilla),
    }
    out
}

fn set(row: usize, instrument: Instrument) -> Edit {
    Edit::SetInstrument { row, instrument }
}

fn shift(text: &str, what: &str) -> Result<Option<f64>, String> {
    let t = text.trim();
    if t.is_empty() {
        return Ok(None);
    }
    t.parse::<f64>()
        .ok()
        .filter(|v| v.is_finite())
        .map(Some)
        .ok_or_else(|| format!("{what} '{t}' is not a number"))
}

/// The one `Edit` a committed cell means (spec §8.4), or the footer's
/// refusal. The tile re-checks that the cell has not moved before it
/// applies this (spec §8.4, "a commit whose cell moved is refused").
pub fn commit(sheet: &Sheet, row: usize, kind: ColumnKind, text: &str) -> Result<Edit, String> {
    let i = instrument(sheet, row).map_err(String::from)?;
    let t = text.trim();
    match kind {
        ColumnKind::Qty => {
            let qty: i64 = t
                .parse()
                .map_err(|_| format!("quantity '{t}' is not a whole number"))?;
            if qty == 0 {
                return Err("quantity must not be zero".into());
            }
            Ok(Edit::SetQty { row, qty })
        }
        ColumnKind::Underlying => {
            if t.is_empty() || t.contains(char::is_whitespace) {
                return Err(format!("underlying '{t}': one word"));
            }
            let u = t.to_ascii_uppercase();
            Ok(set(row, with_vanilla(i, |v| v.underlying = u)))
        }
        ColumnKind::Expiry => {
            let e = parse_expiry(t)?;
            Ok(set(row, with_vanilla(i, |v| v.expiry = e)))
        }
        ColumnKind::Strike => {
            let s = parse_strike(t)?;
            Ok(set(row, with_vanilla(i, |v| v.strike = s)))
        }
        ColumnKind::Type => {
            let k = match t.to_ascii_uppercase().as_str() {
                "C" => OptionKind::Call,
                "P" => OptionKind::Put,
                _ => return Err(format!("type '{t}': C or P")),
            };
            Ok(set(row, with_vanilla(i, |v| v.kind = k)))
        }
        ColumnKind::Barrier => {
            barrier(i).map_err(String::from)?;
            let level = t
                .parse::<f64>()
                .ok()
                .filter(|v| v.is_finite() && *v > 0.0)
                .ok_or_else(|| format!("barrier '{t}' is not a number"))?;
            let mut out = i.clone();
            if let Instrument::Barrier(b) = &mut out {
                b.level = level;
            }
            Ok(set(row, out))
        }
        ColumnKind::BarrierType => {
            barrier(i).map_err(String::from)?;
            let k = parse_barrier_kind(t).ok_or_else(|| format!("barrier type '{t}': UI UO DI DO"))?;
            let mut out = i.clone();
            if let Instrument::Barrier(b) = &mut out {
                b.barrier = k;
            }
            Ok(set(row, out))
        }
        ColumnKind::SpotShift => {
            let own = sheet.shift(row);
            Ok(Edit::SetShift {
                row,
                shift: OwnShifts { spot_pct: shift(t, "spot shift")?, ..own },
            })
        }
        ColumnKind::VolShift => {
            let own = sheet.shift(row);
            Ok(Edit::SetShift {
                row,
                shift: OwnShifts { vol_pts: shift(t, "vol shift")?, ..own },
            })
        }
        ColumnKind::Price
        | ColumnKind::Delta
        | ColumnKind::Gamma
        | ColumnKind::Vega
        | ColumnKind::Theta
        | ColumnKind::Rho
        | ColumnKind::PricedAt
        | ColumnKind::Status => Err(READ_ONLY.into()),
    }
}

/// `up`/`down` in an open numeric editor (spec §8.4): `steps` units of the
/// TEXT's own precision (planning decision 2), a trailing `%` kept, an
/// empty shift nudged from `0`.
pub fn nudge(kind: ColumnKind, text: &str, steps: i64) -> Result<String, String> {
    let t = text.trim();
    match kind {
        ColumnKind::Qty => nudge_text(t, ColumnType::I64, None, steps),
        ColumnKind::Strike | ColumnKind::Barrier => match t.strip_suffix('%') {
            Some(n) => nudge_text(n, ColumnType::F64, None, steps).map(|s| format!("{s}%")),
            None => nudge_text(t, ColumnType::F64, None, steps),
        },
        ColumnKind::SpotShift | ColumnKind::VolShift => {
            nudge_text(if t.is_empty() { "0" } else { t }, ColumnType::F64, None, steps)
        }
        _ => Err("this cell does not nudge".into()),
    }
}

```

- [ ] **Step 4: Run the tests**

Run: `cargo test -p geode-pricer core::cell`
Expected: PASS (seven tests). If `nudge_text`'s own error wording differs from what a test asserts, the test asserts only `is_err()` there — keep it that way; the text belongs to `geode-core`.

- [ ] **Step 5: Gate and commit**

Run: `cargo fmt --check && cargo clippy -p geode-pricer --all-targets -- -D warnings && cargo test -p geode-pricer`

```bash
git add crates/geode-pricer/src/core/cell.rs crates/geode-pricer/src/core/mod.rs
git commit -m "feat(pricer): pure cell editing — open text, commit to one Edit, nudge

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---
### Task 3: `core::tree`, `core::entry`, `core::clip` — expansion, insert placement, history, yank and put

**Files:**
- Create: `crates/geode-pricer/src/core/tree.rs`, `crates/geode-pricer/src/core/entry.rs`, `crates/geode-pricer/src/core/clip.rs`
- Modify: `crates/geode-pricer/src/core/mod.rs`
- Test: each new file's `mod tests`

**Interfaces:**
- Consumes: `Sheet` getters (`len`, `id`, `parent`, `is_package`, `is_line`, `children`, `roots`, `kind`, `instrument`, `qty`, `shift`, `shorthand`), `Place`, `RowSpec`, `LineSpec`, `LineId`.
- Produces:

```rust
// core::tree
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Expansion { /* open: BTreeSet<LineId> */ }
impl Expansion {
    pub fn is_open(&self, id: LineId) -> bool;
    pub fn set(&mut self, id: LineId, open: bool);
    pub fn toggle(&mut self, id: LineId) -> bool;         // the new state
    pub fn open_all(&mut self, sheet: &Sheet);
    pub fn close_all(&mut self);
    pub fn retain_packages(&mut self, sheet: &Sheet);     // drop ids that are no longer packages
    pub fn ids(&self) -> impl Iterator<Item = LineId> + '_;
    pub fn from_ids(ids: impl IntoIterator<Item = LineId>) -> Expansion;
}
pub fn visible_rows(sheet: &Sheet, expansion: &Expansion) -> Vec<usize>;

// core::entry
pub fn place_for(sheet: &Sheet, row: Option<usize>, below: bool) -> Place;
pub fn next_place(place: Place, inserted: &RowSpec) -> Place;
pub fn history(sheet: &Sheet) -> Vec<String>;

// core::clip
pub fn spec_of(sheet: &Sheet, row: usize) -> RowSpec;
pub fn put_place(sheet: &Sheet, row: Option<usize>, below: bool, spec: &RowSpec) -> Place;
```

- [ ] **Step 1: Write the failing tests**

`crates/geode-pricer/src/core/tree.rs`, tests first:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::sheet::tests::{callspread, line, push, spx};
    use geode_core::pricing::OptionKind;

    /// [A, P(L1, L2), B] — flat rows 0..5.
    fn sheet() -> Sheet {
        let mut s = Sheet::new("t");
        push(&mut s, vec![line(spx(5000.0, OptionKind::Call), 1)]);
        push(&mut s, vec![callspread(1)]);
        push(&mut s, vec![line(spx(4000.0, OptionKind::Put), 1)]);
        s
    }

    #[test]
    fn a_closed_package_hides_its_legs_and_an_open_one_shows_them() {
        let s = sheet();
        let mut e = Expansion::default();
        assert_eq!(visible_rows(&s, &e), vec![0, 1, 4]);
        assert!(e.toggle(s.id(1)), "toggle answers the new state");
        assert_eq!(visible_rows(&s, &e), vec![0, 1, 2, 3, 4]);
        e.close_all();
        assert_eq!(visible_rows(&s, &e), vec![0, 1, 4]);
        e.open_all(&s);
        assert!(e.is_open(s.id(1)));
        assert!(!e.is_open(s.id(0)), "open_all opens packages only");
    }

    #[test]
    fn retain_drops_ids_that_are_no_longer_packages() {
        let s = sheet();
        let mut e = Expansion::from_ids([s.id(0), s.id(1), crate::core::LineId(99)]);
        e.retain_packages(&s);
        assert_eq!(e.ids().collect::<Vec<_>>(), vec![s.id(1)]);
    }
}
```

`crates/geode-pricer/src/core/entry.rs`, tests first:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::sheet::tests::{callspread, line, push, spx};
    use crate::core::shorthand::parse;
    use geode_core::pricing::OptionKind;

    /// [A, P(L1, L2), B] — flat rows 0..5.
    fn sheet() -> Sheet {
        let mut s = Sheet::new("t");
        push(&mut s, vec![line(spx(5000.0, OptionKind::Call), 1)]);
        push(&mut s, vec![callspread(1)]);
        push(&mut s, vec![line(spx(4000.0, OptionKind::Put), 1)]);
        s
    }

    #[test]
    fn o_lands_after_the_cursor_row_and_shift_o_before_it() {
        let s = sheet();
        assert_eq!(place_for(&s, None, true), Place::Root { at: 0 }, "an empty sheet");
        assert_eq!(place_for(&s, Some(0), true), Place::Root { at: 1 });
        assert_eq!(place_for(&s, Some(0), false), Place::Root { at: 0 });
        assert_eq!(place_for(&s, Some(1), true), Place::Leg { package: 1, leg: 0 }, "a package row: its first leg");
        assert_eq!(place_for(&s, Some(1), false), Place::Root { at: 1 }, "before the package");
        assert_eq!(place_for(&s, Some(2), true), Place::Leg { package: 1, leg: 1 });
        assert_eq!(place_for(&s, Some(2), false), Place::Leg { package: 1, leg: 0 });
        assert_eq!(place_for(&s, Some(3), true), Place::Leg { package: 1, leg: 2 });
        assert_eq!(place_for(&s, Some(4), true), Place::Root { at: 5 });
    }

    #[test]
    fn the_next_placeholder_follows_what_was_just_inserted() {
        let one = parse("SPX Z26 5000 C").unwrap();
        let cs = parse("SPX Z26 4800/5200 CS").unwrap();
        assert_eq!(next_place(Place::Root { at: 1 }, &one), Place::Root { at: 2 });
        assert_eq!(next_place(Place::Root { at: 1 }, &cs), Place::Root { at: 4 }, "a package and its two legs");
        assert_eq!(
            next_place(Place::Leg { package: 1, leg: 1 }, &one),
            Place::Leg { package: 1, leg: 2 }
        );
    }

    #[test]
    fn history_is_the_roots_shorthand_newest_first_without_repeats() {
        let mut s = sheet();
        push(&mut s, vec![line(spx(5000.0, OptionKind::Call), 1)]); // repeats A's text
        let h = history(&s);
        assert_eq!(
            h,
            vec![
                "SPX Z26 5000 C".to_string(),
                "SPX Z26 4000 P".to_string(),
                "SPX Z26 4800/5200 CS".to_string(),
            ],
            "newest id first; A's repeat collapses into the newest; legs never appear"
        );
    }

    #[test]
    fn a_custom_package_spells_on_several_lines_and_is_left_out_of_history() {
        let mut s = Sheet::new("t");
        push(&mut s, vec![line(spx(5000.0, OptionKind::Call), 1)]);
        push(&mut s, vec![line(spx(4000.0, OptionKind::Put), 1)]);
        s.apply(crate::core::Edit::Group {
            first: 0,
            count: 2,
            template: crate::core::Template::Custom,
            id: None,
        })
        .unwrap();
        assert!(history(&s).is_empty(), "one entry field line cannot hold it");
    }
}
```

`crates/geode-pricer/src/core/clip.rs`, tests first:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::sheet::tests::{callspread, line, push, spx};
    use crate::core::shorthand::parse;
    use geode_core::pricing::OptionKind;

    fn sheet() -> Sheet {
        let mut s = Sheet::new("t");
        push(&mut s, vec![line(spx(5000.0, OptionKind::Call), 3)]);
        push(&mut s, vec![callspread(-5)]);
        push(&mut s, vec![line(spx(4000.0, OptionKind::Put), 1)]);
        s
    }

    #[test]
    fn a_yanked_row_is_its_spec_and_a_package_takes_its_legs() {
        let s = sheet();
        assert_eq!(spec_of(&s, 0), line(spx(5000.0, OptionKind::Call), 3));
        assert_eq!(spec_of(&s, 1), callspread(-5), "the package with its legs");
        let RowSpec::Line(leg) = spec_of(&s, 2) else {
            panic!("a leg yanks as a line")
        };
        assert_eq!(leg.qty, -5);
    }

    #[test]
    fn a_put_package_lands_at_a_root_boundary_and_a_line_follows_o() {
        let s = sheet();
        let cs = parse("SPX Z26 4800/5200 CS").unwrap();
        let one = parse("SPX Z26 5000 C").unwrap();
        assert_eq!(put_place(&s, Some(2), true, &cs), Place::Root { at: 4 }, "after the leg's whole package");
        assert_eq!(put_place(&s, Some(2), false, &cs), Place::Root { at: 1 });
        assert_eq!(put_place(&s, Some(1), true, &cs), Place::Root { at: 4 });
        assert_eq!(put_place(&s, None, true, &cs), Place::Root { at: 0 });
        assert_eq!(put_place(&s, Some(2), true, &one), Place::Leg { package: 1, leg: 1 });
        assert_eq!(put_place(&s, Some(1), true, &one), Place::Leg { package: 1, leg: 0 });
    }
}
```

Register the modules in `crates/geode-pricer/src/core/mod.rs` (alphabetical: `cell, clip, columns, edit, entry, sheet, shorthand, storage, template, tree, views`) and add `pub use tree::{Expansion, visible_rows};`.

- [ ] **Step 2: Run to see them fail**

Run: `cargo test -p geode-pricer core::tree core::entry core::clip` (three runs if the filter takes one: `cargo test -p geode-pricer core::`)
Expected: FAIL to compile — the functions do not exist.

- [ ] **Step 3: Implement `core::tree`**

Above its tests:

```rust
//! Which package rows are open (line-pricer spec §8.2, §8.5's tree keys)
//! and the flat rows that are therefore visible. Keyed by `LineId`, so
//! an edit that moves rows never opens or closes the wrong package.

use crate::core::sheet::{LineId, Sheet};
use std::collections::BTreeSet;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Expansion {
    open: BTreeSet<LineId>,
}

impl Expansion {
    pub fn from_ids(ids: impl IntoIterator<Item = LineId>) -> Expansion {
        Expansion {
            open: ids.into_iter().collect(),
        }
    }

    pub fn is_open(&self, id: LineId) -> bool {
        self.open.contains(&id)
    }

    pub fn set(&mut self, id: LineId, open: bool) {
        if open {
            self.open.insert(id);
        } else {
            self.open.remove(&id);
        }
    }

    /// Flip one package; answers the new state.
    pub fn toggle(&mut self, id: LineId) -> bool {
        let open = !self.is_open(id);
        self.set(id, open);
        open
    }

    pub fn open_all(&mut self, sheet: &Sheet) {
        self.open = (0..sheet.len())
            .filter(|r| sheet.is_package(*r))
            .map(|r| sheet.id(r))
            .collect();
    }

    pub fn close_all(&mut self) {
        self.open.clear();
    }

    /// Forget ids that no longer name a package (removed, or ungrouped),
    /// so the session record never carries dead ids.
    pub fn retain_packages(&mut self, sheet: &Sheet) {
        self.open
            .retain(|id| sheet.index_of(*id).is_some_and(|r| sheet.is_package(r)));
    }

    pub fn ids(&self) -> impl Iterator<Item = LineId> + '_ {
        self.open.iter().copied()
    }
}

/// Flat rows in sheet order, a leg shown only under an open package.
pub fn visible_rows(sheet: &Sheet, expansion: &Expansion) -> Vec<usize> {
    (0..sheet.len())
        .filter(|r| match sheet.parent(*r) {
            None => true,
            Some(p) => expansion.is_open(sheet.id(p)),
        })
        .collect()
}
```

- [ ] **Step 4: Implement `core::entry`**

```rust
//! The entry field's pure half (line-pricer spec §8.4): where `o` and
//! `shift+o` land (planning decision 12), where the next placeholder
//! opens after a successful `enter`, and the `up`/`down` history.

use crate::core::sheet::{Place, RowSpec, Sheet};

/// Where a new row lands relative to the cursor row. `row` is a flat row
/// index; `None` is an empty sheet (or no cursor).
pub fn place_for(sheet: &Sheet, row: Option<usize>, below: bool) -> Place {
    let Some(row) = row else {
        return Place::Root { at: 0 };
    };
    if let Some(p) = sheet.parent(row) {
        let leg = row - p - 1;
        return Place::Leg {
            package: p,
            leg: if below { leg + 1 } else { leg },
        };
    }
    if sheet.is_package(row) {
        return if below {
            Place::Leg { package: row, leg: 0 }
        } else {
            Place::Root { at: row }
        };
    }
    Place::Root {
        at: if below { row + 1 } else { row },
    }
}

/// Flat rows `spec` occupies once inserted: a line one, a package itself
/// plus its legs.
fn span(spec: &RowSpec) -> usize {
    match spec {
        RowSpec::Line(_) => 1,
        RowSpec::Package { legs, .. } => 1 + legs.len(),
    }
}

/// The placeholder after `inserted` landed at `place`, so a book of lines
/// is typed without another `o` (spec §8.4). A package at a leg place is
/// refused by `apply`, so that pair answers `place` unchanged.
pub fn next_place(place: Place, inserted: &RowSpec) -> Place {
    match (place, inserted) {
        (Place::Root { at }, spec) => Place::Root { at: at + span(spec) },
        (Place::Leg { package, leg }, RowSpec::Line(_)) => Place::Leg { package, leg: leg + 1 },
        (p @ Place::Leg { .. }, RowSpec::Package { .. }) => p,
    }
}

/// The entry field's history (spec §8.4: "the sheet's own lines, most
/// recent first"): every ROOT row's shorthand, newest id first, a repeat
/// kept only at its newest. A custom package spells on several lines and
/// cannot be one entry, so it is left out.
pub fn history(sheet: &Sheet) -> Vec<String> {
    let mut roots: Vec<usize> = sheet.roots().collect();
    roots.sort_by_key(|r| std::cmp::Reverse(sheet.id(*r)));
    let mut out: Vec<String> = Vec::new();
    for r in roots {
        let text = sheet.shorthand(r);
        if text.is_empty() || text.contains('\n') || out.contains(&text) {
            continue;
        }
        out.push(text);
    }
    out
}
```

- [ ] **Step 5: Implement `core::clip`**

```rust
//! Yank and put (line-pricer spec §8.5's `y y`, `d d`, `p`/`shift+p`): a
//! row as the `RowSpec` it would be typed as — so a put takes fresh ids
//! and re-requests, never a copy of results — and where a put lands
//! (planning decision 12).

use crate::core::entry::place_for;
use crate::core::sheet::{LineSpec, Place, RowKind, RowSpec, Sheet};

fn line_spec(sheet: &Sheet, row: usize) -> LineSpec {
    LineSpec {
        instrument: sheet
            .instrument(row)
            .expect("a line has an instrument")
            .clone(),
        qty: sheet.qty(row),
        shift: sheet.shift(row),
    }
}

/// The row as a spec: a line (a leg included) as a line, a package with
/// its legs.
pub fn spec_of(sheet: &Sheet, row: usize) -> RowSpec {
    match sheet.kind(row) {
        RowKind::Package { template } => RowSpec::Package {
            template,
            legs: sheet.children(row).map(|l| line_spec(sheet, l)).collect(),
        },
        RowKind::Line | RowKind::Underlying => RowSpec::Line(line_spec(sheet, row)),
    }
}

/// Where a put lands. A package always goes to a root boundary — before
/// or after the cursor's whole root block — since depth is at most two;
/// a line follows `o`'s rule.
pub fn put_place(sheet: &Sheet, row: Option<usize>, below: bool, spec: &RowSpec) -> Place {
    match (spec, row) {
        (RowSpec::Line(_), _) | (_, None) => place_for(sheet, row, below),
        (RowSpec::Package { .. }, Some(row)) => {
            let root = sheet.parent(row).unwrap_or(row);
            Place::Root {
                at: if below {
                    sheet.children(root).end.max(root + 1)
                } else {
                    root
                },
            }
        }
    }
}
```

- [ ] **Step 6: Run the tests**

Run: `cargo test -p geode-pricer core::`
Expected: PASS. (`history`'s expected strings assume `render_line` prints a quantity of `1` with no prefix and upper-case tokens, which `shorthand`'s round-trip tests pin; if the committed renderer spells them differently, correct the EXPECTED strings to the renderer's, never the renderer.)

- [ ] **Step 7: Gate and commit**

Run: `cargo fmt --check && cargo clippy -p geode-pricer --all-targets -- -D warnings && cargo test -p geode-pricer`

```bash
git add crates/geode-pricer/src/core/
git commit -m "feat(pricer): pure expansion, insert placement, history, yank and put

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---
### Task 4: `core::undo` and `core::commands` — the LIFO stack and the `:` vocabulary

**Files:**
- Create: `crates/geode-pricer/src/core/undo.rs`, `crates/geode-pricer/src/core/commands.rs`
- Modify: `crates/geode-pricer/src/core/mod.rs`
- Test: each new file's `mod tests`

**Interfaces:**
- Consumes: `Sheet::apply`, `Sheet::undo(&Undo) -> Result<Undo, EditError>` (not atomic; answers the redo), `Undo`, `EditError`, `Refresh`, `geode_core::source_config::parse_duration(&str) -> Option<Duration>`.
- Produces:

```rust
// core::undo
pub const UNDO_DEPTH: usize = 100;
#[derive(Debug, Default)]
pub struct UndoStack { /* done: VecDeque<Undo>, undone: Vec<Undo> */ }
impl UndoStack {
    pub fn record(&mut self, undo: Undo);
    pub fn undo(&mut self, sheet: &mut Sheet) -> Result<bool, EditError>; // Ok(false): nothing to undo
    pub fn redo(&mut self, sheet: &mut Sheet) -> Result<bool, EditError>;
    pub fn can_undo(&self) -> bool;
    pub fn can_redo(&self) -> bool;
    pub fn clear(&mut self);
}

// core::commands
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShiftField { Spot, Vol }
#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    View(String),
    Shift { field: ShiftField, value: Option<f64> },          // None clears
    Spot { underlying: Option<String>, level: Option<f64> },  // (None, None): clear all
    Price,
    Refresh(Refresh),
    Group(Option<usize>),
    Ungroup,
}
pub const VERBS: [&str; 7];
pub const NOT_BUILT: [&str; 4];
pub fn parse(line: &str) -> Result<Command, String>;
pub fn completions(line: &str, cursor: usize, views: &[String], underlyings: &[String]) -> Vec<String>;
```

- [ ] **Step 1: Write the failing undo tests**

`crates/geode-pricer/src/core/undo.rs`, tests first. The first two are the Part 2 final review's untested cases (spec §17, "deferred minors": a package hopping upward and a multi-edit redo):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::edit::Edit;
    use crate::core::sheet::tests::{at, callspread, line, push, result, spx};
    use crate::core::sheet::{LineId, Place};
    use geode_core::pricing::OptionKind;

    fn ids(s: &Sheet) -> Vec<LineId> {
        (0..s.len()).map(|r| s.id(r)).collect()
    }

    fn apply(stack: &mut UndoStack, s: &mut Sheet, e: Edit) {
        let undo = s.apply(e).unwrap();
        stack.record(undo);
    }

    #[test]
    fn a_package_hopping_upward_undoes_and_redoes_to_the_same_order() {
        let mut s = Sheet::new("t");
        push(&mut s, vec![line(spx(5000.0, OptionKind::Call), 1)]);
        push(&mut s, vec![line(spx(4000.0, OptionKind::Put), 1)]);
        push(&mut s, vec![callspread(1)]);
        let before = ids(&s);
        let mut stack = UndoStack::default();
        apply(&mut stack, &mut s, Edit::Move { row: 2, delta: -1 });
        let moved = ids(&s);
        assert_eq!(s.children(1), 2..4, "the package moved up with its legs");
        assert_ne!(moved, before);

        assert_eq!(stack.undo(&mut s), Ok(true));
        assert_eq!(ids(&s), before);
        assert_eq!(stack.redo(&mut s), Ok(true));
        assert_eq!(ids(&s), moved, "redo lands the same order, legs contiguous");
        assert_eq!(stack.undo(&mut s), Ok(true));
        assert_eq!(ids(&s), before, "and it undoes again");
    }

    #[test]
    fn a_multi_edit_undo_redoes_whole_with_the_same_ids_and_results() {
        let mut s = Sheet::new("t");
        let mut stack = UndoStack::default();
        apply(
            &mut stack,
            &mut s,
            Edit::Insert {
                place: Place::Root { at: 0 },
                rows: vec![
                    line(spx(5000.0, OptionKind::Call), 1),
                    callspread(-2),
                    line(spx(4000.0, OptionKind::Put), 3),
                ],
            },
        );
        let answers: Vec<_> = (0..s.len())
            .filter(|r| s.is_line(*r))
            .map(|r| (s.id(r), s.revision(r), Ok(result(r as f64 + 1.0))))
            .collect();
        s.deliver_all(answers, at(0));
        let before = ids(&s);
        let prices: Vec<_> = (0..s.len()).map(|r| s.result(r).map(|x| x.price)).collect();

        assert_eq!(stack.undo(&mut s), Ok(true));
        assert!(s.is_empty(), "one undo takes back the whole insert");
        assert_eq!(stack.redo(&mut s), Ok(true));
        assert_eq!(ids(&s), before, "redo reinstates the same ids");
        let after: Vec<_> = (0..s.len()).map(|r| s.result(r).map(|x| x.price)).collect();
        assert_eq!(after, prices, "…and their results: nothing is re-requested");
        assert!(s.stale_lines().next().is_none());
    }

    #[test]
    fn a_new_edit_clears_the_redo_side() {
        let mut s = Sheet::new("t");
        let mut stack = UndoStack::default();
        push(&mut s, vec![line(spx(5000.0, OptionKind::Call), 1)]);
        apply(&mut stack, &mut s, Edit::SetQty { row: 0, qty: 2 });
        stack.undo(&mut s).unwrap();
        assert!(stack.can_redo());
        apply(&mut stack, &mut s, Edit::SetQty { row: 0, qty: 5 });
        assert!(!stack.can_redo(), "a fresh edit forks history");
        assert_eq!(stack.redo(&mut s), Ok(false));
        assert_eq!(s.qty(0), 5);
    }

    #[test]
    fn the_stack_keeps_the_newest_hundred() {
        let mut s = Sheet::new("t");
        push(&mut s, vec![line(spx(5000.0, OptionKind::Call), 1)]);
        let mut stack = UndoStack::default();
        for q in 2..(UNDO_DEPTH as i64 + 12) {
            apply(&mut stack, &mut s, Edit::SetQty { row: 0, qty: q });
        }
        let mut n = 0;
        while stack.undo(&mut s).unwrap() {
            n += 1;
        }
        assert_eq!(n, UNDO_DEPTH);
        assert_eq!(s.qty(0), 11, "the oldest eleven edits fell off the bottom");
    }

    /// Why every tile edit goes through the stack (Part 3 global
    /// constraints): an inverse recorded against rows that moved behind
    /// the stack's back is refused, `Sheet::undo` is not atomic, and the
    /// only safe state afterwards is an empty history.
    #[test]
    fn an_inverse_refused_mid_undo_clears_both_sides() {
        let mut s = Sheet::new("t");
        let mut stack = UndoStack::default();
        apply(
            &mut stack,
            &mut s,
            Edit::Insert {
                place: Place::Root { at: 0 },
                rows: vec![line(spx(5000.0, OptionKind::Call), 1)],
            },
        );
        s.apply(Edit::Remove { at: 0 }).unwrap(); // behind the stack's back
        assert!(stack.undo(&mut s).is_err());
        assert!(!stack.can_undo() && !stack.can_redo());
    }
}
```

- [ ] **Step 2: Write the failing command tests**

`crates/geode-pricer/src/core/commands.rs`, tests first:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn every_verb_parses_its_arguments() {
        assert_eq!(parse("view barrier"), Ok(Command::View("barrier".into())));
        assert_eq!(
            parse("shift spot 2"),
            Ok(Command::Shift { field: ShiftField::Spot, value: Some(2.0) })
        );
        assert_eq!(
            parse("shift vol -1.5"),
            Ok(Command::Shift { field: ShiftField::Vol, value: Some(-1.5) })
        );
        assert_eq!(
            parse("shift vol clear"),
            Ok(Command::Shift { field: ShiftField::Vol, value: None })
        );
        assert_eq!(
            parse("spot spx 5100"),
            Ok(Command::Spot { underlying: Some("SPX".into()), level: Some(5100.0) })
        );
        assert_eq!(
            parse("spot SPX clear"),
            Ok(Command::Spot { underlying: Some("SPX".into()), level: None })
        );
        assert_eq!(parse("spot clear"), Ok(Command::Spot { underlying: None, level: None }));
        assert_eq!(parse("price"), Ok(Command::Price));
        assert_eq!(parse("refresh off"), Ok(Command::Refresh(Refresh::Off)));
        assert_eq!(parse("refresh default"), Ok(Command::Refresh(Refresh::Default)));
        assert_eq!(
            parse("refresh 500ms"),
            Ok(Command::Refresh(Refresh::Every(Duration::from_millis(500))))
        );
        assert_eq!(parse("group"), Ok(Command::Group(None)));
        assert_eq!(parse("group 3"), Ok(Command::Group(Some(3))));
        assert_eq!(parse("ungroup"), Ok(Command::Ungroup));
    }

    #[test]
    fn bad_arguments_answer_the_usage_and_part_4_verbs_refuse_by_name() {
        assert_eq!(parse(""), Err("empty command".into()));
        assert_eq!(parse("view"), Err("usage: view <name>".into()));
        assert_eq!(parse("shift up 2"), Err("usage: shift spot|vol <n>|clear".into()));
        assert_eq!(parse("shift spot x"), Err("shift: 'x' is not a number".into()));
        assert_eq!(parse("spot SPX -1"), Err("spot: 'SPX -1' needs a positive level".into()));
        assert_eq!(parse("spot"), Err("usage: spot <underlying> <level>|clear".into()));
        assert_eq!(parse("refresh 0s"), Err("usage: refresh <duration>|off|default".into()));
        assert_eq!(parse("refresh soon"), Err("usage: refresh <duration>|off|default".into()));
        assert_eq!(parse("group 0"), Err("usage: group [count]".into()));
        assert_eq!(parse("price now"), Err("usage: price".into()));
        for verb in NOT_BUILT {
            assert_eq!(parse(verb), Err(format!(":{verb} is not built yet")));
        }
        assert_eq!(parse("bogus"), Err("unknown command 'bogus'".into()));
    }

    #[test]
    fn completions_offer_each_positions_vocabulary_unfiltered() {
        let views = vec!["vanilla".to_string(), "barrier".to_string()];
        let unds = vec!["NDX".to_string(), "SPX".to_string()];
        assert_eq!(completions("", 0, &views, &unds), VERBS.map(String::from).to_vec());
        assert_eq!(completions("vi", 2, &views, &unds), VERBS.map(String::from).to_vec(), "the shell ranks");
        assert_eq!(completions("view ", 5, &views, &unds), views);
        assert_eq!(completions("shift ", 6, &views, &unds), vec!["spot", "vol"]);
        assert_eq!(completions("shift spot ", 11, &views, &unds), vec!["clear"]);
        assert_eq!(completions("spot ", 5, &views, &unds), vec!["NDX", "SPX", "clear"]);
        assert_eq!(completions("spot SPX ", 9, &views, &unds), vec!["clear"]);
        assert_eq!(completions("refresh ", 8, &views, &unds), vec!["off", "default"]);
        assert!(completions("price ", 6, &views, &unds).is_empty());
    }

    #[test]
    fn a_cursor_off_a_char_boundary_does_not_panic() {
        let _ = completions("spot é", 6, &[], &[]);
        let _ = completions("spot é", 99, &[], &[]);
    }
}
```

Register both modules in `core/mod.rs` and add `pub use undo::{UNDO_DEPTH, UndoStack};`. (`commands` is used by path: `core::commands::parse`.)

- [ ] **Step 3: Run to see them fail**

Run: `cargo test -p geode-pricer core::undo core::commands` (or `cargo test -p geode-pricer core::`)
Expected: FAIL to compile.

- [ ] **Step 4: Implement `core::undo`**

```rust
//! The tile's undo history (line-pricer spec §6.2, §8.5's `u`/`ctrl+r`):
//! at most [`UNDO_DEPTH`] entries, strictly LIFO. Every inverse is
//! recorded against the exact rows the edit left, so NOTHING may edit
//! the sheet except through a recorded `apply` — a `Restore` trusts its
//! records' state and revision (spec §17's Part 3 obligation). When an
//! inverse is refused anyway, `Sheet::undo` is not atomic, so the whole
//! history is dropped rather than left pointing at rows that moved.

use crate::core::edit::{EditError, Undo};
use crate::core::sheet::Sheet;
use std::collections::VecDeque;

pub const UNDO_DEPTH: usize = 100;

#[derive(Debug, Default)]
pub struct UndoStack {
    done: VecDeque<Undo>,
    undone: Vec<Undo>,
}

impl UndoStack {
    /// A fresh edit: onto the done side, the redo side forgotten (a new
    /// edit forks history), the oldest entry dropped past the depth.
    pub fn record(&mut self, undo: Undo) {
        self.undone.clear();
        self.done.push_back(undo);
        if self.done.len() > UNDO_DEPTH {
            self.done.pop_front();
        }
    }

    /// Take back the newest edit. `Ok(false)` when there is none.
    pub fn undo(&mut self, sheet: &mut Sheet) -> Result<bool, EditError> {
        let Some(undo) = self.done.pop_back() else {
            return Ok(false);
        };
        match sheet.undo(&undo) {
            Ok(redo) => {
                self.undone.push(redo);
                Ok(true)
            }
            Err(e) => {
                self.clear();
                Err(e)
            }
        }
    }

    /// Re-apply the newest undone edit. `Ok(false)` when there is none.
    pub fn redo(&mut self, sheet: &mut Sheet) -> Result<bool, EditError> {
        let Some(redo) = self.undone.pop() else {
            return Ok(false);
        };
        match sheet.undo(&redo) {
            Ok(undo) => {
                self.done.push_back(undo);
                Ok(true)
            }
            Err(e) => {
                self.clear();
                Err(e)
            }
        }
    }

    pub fn can_undo(&self) -> bool {
        !self.done.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.undone.is_empty()
    }

    pub fn clear(&mut self) {
        self.done.clear();
        self.undone.clear();
    }
}
```

- [ ] **Step 5: Implement `core::commands`**

```rust
//! The tile's `:` vocabulary (line-pricer spec §8.6), pure: parse and
//! completion. Every verb changes only this tile. `:e`, `:name`, `:new`
//! and `:rm` are Part 4's; they parse to a refusal that names them
//! (Part 3 planning decision 9) rather than "unknown command".

use crate::core::sheet::Refresh;
use geode_core::source_config::parse_duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShiftField {
    Spot,
    Vol,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    View(String),
    /// `value: None` clears the sheet-wide value.
    Shift { field: ShiftField, value: Option<f64> },
    /// `(Some(u), Some(level))` sets; `(Some(u), None)` clears one;
    /// `(None, None)` clears every override.
    Spot { underlying: Option<String>, level: Option<f64> },
    Price,
    Refresh(Refresh),
    Group(Option<usize>),
    Ungroup,
}

pub const VERBS: [&str; 7] = ["view", "shift", "spot", "price", "refresh", "group", "ungroup"];
pub const NOT_BUILT: [&str; 4] = ["e", "name", "new", "rm"];

const SHIFT_USAGE: &str = "usage: shift spot|vol <n>|clear";
const SPOT_USAGE: &str = "usage: spot <underlying> <level>|clear";
const REFRESH_USAGE: &str = "usage: refresh <duration>|off|default";

pub fn parse(line: &str) -> Result<Command, String> {
    let words: Vec<&str> = line.split_whitespace().collect();
    match words.as_slice() {
        [] => Err("empty command".into()),
        ["view", name] => Ok(Command::View((*name).to_string())),
        ["view", ..] => Err("usage: view <name>".into()),
        ["shift", field, value] => {
            let field = match *field {
                "spot" => ShiftField::Spot,
                "vol" => ShiftField::Vol,
                _ => return Err(SHIFT_USAGE.into()),
            };
            let value = match *value {
                "clear" => None,
                v => Some(
                    v.parse::<f64>()
                        .ok()
                        .filter(|x| x.is_finite())
                        .ok_or_else(|| format!("shift: '{v}' is not a number"))?,
                ),
            };
            Ok(Command::Shift { field, value })
        }
        ["shift", ..] => Err(SHIFT_USAGE.into()),
        ["spot", "clear"] => Ok(Command::Spot { underlying: None, level: None }),
        ["spot", und, "clear"] => Ok(Command::Spot {
            underlying: Some(und.to_ascii_uppercase()),
            level: None,
        }),
        ["spot", und, level] => {
            let level = level
                .parse::<f64>()
                .ok()
                .filter(|x| x.is_finite() && *x > 0.0)
                .ok_or_else(|| format!("spot: '{und} {level}' needs a positive level"))?;
            Ok(Command::Spot {
                underlying: Some(und.to_ascii_uppercase()),
                level: Some(level),
            })
        }
        ["spot", ..] => Err(SPOT_USAGE.into()),
        ["price"] => Ok(Command::Price),
        ["price", ..] => Err("usage: price".into()),
        ["refresh", "off"] => Ok(Command::Refresh(Refresh::Off)),
        ["refresh", "default"] => Ok(Command::Refresh(Refresh::Default)),
        ["refresh", d] => parse_duration(d)
            .filter(|d| !d.is_zero())
            .map(|d| Command::Refresh(Refresh::Every(d)))
            .ok_or_else(|| REFRESH_USAGE.into()),
        ["refresh", ..] => Err(REFRESH_USAGE.into()),
        ["group"] => Ok(Command::Group(None)),
        ["group", n] => n
            .parse::<usize>()
            .ok()
            .filter(|n| *n >= 1)
            .map(|n| Command::Group(Some(n)))
            .ok_or_else(|| "usage: group [count]".into()),
        ["group", ..] => Err("usage: group [count]".into()),
        ["ungroup"] => Ok(Command::Ungroup),
        ["ungroup", ..] => Err("usage: ungroup".into()),
        [verb, ..] if NOT_BUILT.contains(verb) => Err(format!(":{verb} is not built yet")),
        [other, ..] => Err(format!("unknown command '{other}'")),
    }
}

/// The bare words valid at `cursor` — the position's whole vocabulary,
/// unfiltered; the shell ranks (`TileContent::completions`).
pub fn completions(line: &str, cursor: usize, views: &[String], underlyings: &[String]) -> Vec<String> {
    let mut end = cursor.min(line.len());
    while !line.is_char_boundary(end) {
        end -= 1;
    }
    let mut words: Vec<&str> = line[..end].split(char::is_whitespace).collect();
    words.pop(); // the word under the cursor
    words.retain(|w| !w.is_empty());
    let strs = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    match words.as_slice() {
        [] => strs(&VERBS),
        ["view"] => views.to_vec(),
        ["shift"] => strs(&["spot", "vol"]),
        ["shift", _] => strs(&["clear"]),
        ["spot"] => underlyings.iter().cloned().chain(["clear".to_string()]).collect(),
        ["spot", _] => strs(&["clear"]),
        ["refresh"] => strs(&["off", "default"]),
        _ => Vec::new(),
    }
}
```

- [ ] **Step 6: Run the tests**

Run: `cargo test -p geode-pricer core::`
Expected: PASS. If `parse_duration("0s")` answers `None` rather than `Some(ZERO)`, the `refresh 0s` expectation still holds (both routes end at the usage).

- [ ] **Step 7: Gate and commit**

Run: `cargo fmt --check && cargo clippy -p geode-pricer --all-targets -- -D warnings && cargo test -p geode-pricer`

```bash
git add crates/geode-pricer/src/core/
git commit -m "feat(pricer): LIFO undo stack and the : vocabulary

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---
### Task 5: UI dependencies, the `SheetStore` seam, `GridModel`, `Paints` and the grid bench

**Files:**
- Modify: `crates/geode-pricer/Cargo.toml`, `crates/geode-pricer/src/lib.rs`
- Create: `crates/geode-pricer/src/store.rs`, `crates/geode-pricer/src/grid.rs`, `crates/geode-pricer/src/paint.rs`
- Modify: `crates/geode-pricer/benches/core.rs`
- Test: `mod tests` in each new file

**Interfaces:**
- Consumes: `core::{cell_text, CellState, ColumnPlan, PlannedColumn, ColumnKind, Expansion, visible_rows, Place, Sheet, LineId, to_rows, from_rows}`, `geode_core::clock::Clock`, `geode_core::document::DocumentRows`, `geode_shell::shell::colours::{to_rgb, to_hsla, over}`, `geode_shell::shell::chip::{chip_paint, Tone}`, `geode_core::colour::{readable_on, contrast_ratio, READABLE_RATIO, Rgb}`.
- Produces:

```rust
// store.rs
pub enum Loaded { Rows(DocumentRows), Missing, Pending }
pub trait SheetStore {
    fn load(&self, name: &str) -> Loaded;
    fn save(&self, name: &str, rows: DocumentRows) -> bool;
    fn contains(&self, name: &str) -> bool;
}
#[derive(Clone, Default)]
pub struct MemorySheetStore { /* Rc-shared map + test knobs */ }
impl MemorySheetStore {
    pub fn get(&self, name: &str) -> Option<DocumentRows>;
    pub fn save_count(&self) -> usize;
    pub fn set_refusing(&self, refusing: bool);
    pub fn set_pending(&self, pending: bool);
}

// grid.rs
pub struct GridColumn { pub label: SharedString, pub width: f32, pub right: bool, pub kind: ColumnKind, pub editable: bool }
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GridRowKind { Line, Leg, Package { open: bool }, Entry }
pub struct GridCell { pub text: SharedString, pub state: CellState }
pub struct GridRow { pub kind: GridRowKind, pub row: Option<usize>, pub id: Option<LineId>, pub depth: usize, pub tree: SharedString, pub cells: Vec<GridCell> }
#[derive(Default)]
pub struct GridModel { pub columns: Vec<GridColumn>, pub rows: Vec<GridRow> }
impl GridModel {
    pub fn build(sheet: &Sheet, expansion: &Expansion, plan: &ColumnPlan, entry: Option<Place>, clock: Clock) -> GridModel;
    pub fn grid_row_of(&self, id: LineId) -> Option<usize>;
    pub fn entry_row(&self) -> Option<usize>;
}

// paint.rs
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Paints { pub own: Hsla, pub muted: Hsla, pub danger: Hsla, pub package_ground: Hsla, pub package_own: Hsla, pub package_muted: Hsla, pub package_danger: Hsla }
impl Paints {
    pub fn derive(theme: &Theme) -> Paints;
    pub fn text(&self, state: CellState, package: bool) -> Hsla;
}
```

- [ ] **Step 1: Dependencies and module list**

Replace `crates/geode-pricer/Cargo.toml`'s dependency comment and tables with:

```toml
# The pure core (`core`) needs `geode-core` and `chrono` alone. The tile
# (Part 3) adds `geode-shell` for the module-hosting contract (`TileContent`,
# `Frame`, the keymap and action vocabulary, `ChoiceList`, the chip, colour,
# list-row and control doors), `geode-data` for `DataHandle` alone — the one
# door a module asks for data (pricing) through — `gpui`/`gpui-component`
# for the tile and its `DataTable`, `toml` for the session record, and
# `tracing` for the delivery bug log (spec §10.1). Never `geode-pricing`: a
# module reaches the pricer only through the data tier's request door
# (PHILOSOPHY §1, "In-process calculation"); never a sibling module.
[dependencies]
geode-core.workspace = true
geode-shell.workspace = true
geode-data = { path = "../geode-data" }
gpui.workspace = true
gpui-component.workspace = true
chrono = "0.4.42"
toml = "1.1.4"
tracing.workspace = true

# Dev-dependencies ask for the same geode-* features `--workspace`
# resolves (test-feature parity, 2026-09-19), or this crate builds a
# private copy of the stack.
[dev-dependencies]
geode-core = { workspace = true, features = ["test-support"] }
geode-shell = { workspace = true, features = ["test-support"] }
geode-data = { path = "../geode-data", features = ["test-support"] }
gpui = { workspace = true, features = ["test-support"] }
criterion = "0.8.2"
```

Keep `[lib] bench = false` and the `[[bench]] name = "core" harness = false` table as they are.

Replace `crates/geode-pricer/src/lib.rs` with:

```rust
//! The line pricer module (line-pricer spec §8): a tile whose rows are
//! option lines and packages, priced through the data tier's pricing
//! request. [`core`] is the pure half — it names no element, entity,
//! window, data service or pricing implementation; the rest is the tile.

pub mod core;

pub mod grid;
pub mod paint;
pub mod store;
```

(Tasks 6–12 add `content`, `delegate`, `header`, `popup`, `tile` and `init`.)

- [ ] **Step 2: Write the failing store tests**

`crates/geode-pricer/src/store.rs`, tests first:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::sheet::tests::{callspread, line, push, spx};
    use crate::core::{Sheet, from_rows, to_rows};
    use geode_core::pricing::OptionKind;

    fn rows() -> DocumentRows {
        let mut s = Sheet::new("book");
        push(&mut s, vec![line(spx(5000.0, OptionKind::Call), 1), callspread(-2)]);
        to_rows(&s).expect("a non-empty sheet has rows")
    }

    #[test]
    fn a_saved_sheet_loads_back_and_an_unknown_name_is_missing() {
        let store = MemorySheetStore::default();
        assert!(matches!(store.load("book"), Loaded::Missing));
        assert!(!store.contains("book"));
        assert!(store.save("book", rows()));
        assert!(store.contains("book"));
        let Loaded::Rows(back) = store.load("book") else {
            panic!("saved rows load")
        };
        assert_eq!(from_rows("book", &back).unwrap().len(), 4);
        assert_eq!(store.save_count(), 1);
    }

    #[test]
    fn a_refusing_store_keeps_nothing_and_a_pending_one_answers_pending() {
        let store = MemorySheetStore::default();
        store.set_refusing(true);
        assert!(!store.save("book", rows()));
        assert!(store.get("book").is_none());
        store.set_pending(true);
        assert!(matches!(store.load("book"), Loaded::Pending));
    }

    #[test]
    fn clones_share_one_map() {
        let a = MemorySheetStore::default();
        let b = a.clone();
        a.save("book", rows());
        assert!(b.contains("book"), "the factory and every tile see one store");
    }
}
```

- [ ] **Step 3: Implement `store.rs`**

Above the tests:

```rust
//! Where a sheet lives between tiles (line-pricer spec §7.1): the tile
//! never holds a document request of its own, only this seam.
//!
//! **The shape is Part 3's** (planning decision 7): `load` answers at
//! once when it can and `Pending` when the answer is on its way — Part
//! 4's DuckDB store answers `Pending` and delivers the rows through the
//! tile's `loaded`, reached from its `Delivery::Query` arm. `save` is one
//! whole-sheet publish; `false` is a refusal the tile shows and retries on
//! the next edit burst (spec §7.3). A zero-row sheet is never saved
//! (`to_rows` answers `None`; spec §7.2).
//!
//! [`MemorySheetStore`] is the only implementation until Part 4 — the
//! app's for this part and the tests' fake. A sheet in it lives for the
//! process, not across a restart.

use geode_core::document::DocumentRows;
use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::rc::Rc;

/// What a `load` answered.
#[derive(Debug)]
pub enum Loaded {
    Rows(DocumentRows),
    /// No document under that name: an empty sheet, not an error (§7.2).
    Missing,
    /// On its way; the tile paints `loading` until `PricerTile::loaded`.
    Pending,
}

pub trait SheetStore {
    fn load(&self, name: &str) -> Loaded;
    /// Publish the whole sheet. `false`: refused, nothing written.
    fn save(&self, name: &str, rows: DocumentRows) -> bool;
    /// Whether a document exists under `name` (the `untitled-N` rule).
    fn contains(&self, name: &str) -> bool;
}

/// The process-lifetime store. Clones share one map, so the factory and
/// every tile it builds see the same sheets. The three knobs exist for
/// the tile's tests (a refused save, a pending load, a save count); the
/// app never turns them.
#[derive(Clone, Default)]
pub struct MemorySheetStore {
    sheets: Rc<RefCell<BTreeMap<String, DocumentRows>>>,
    saves: Rc<Cell<usize>>,
    refusing: Rc<Cell<bool>>,
    pending: Rc<Cell<bool>>,
}

impl MemorySheetStore {
    pub fn get(&self, name: &str) -> Option<DocumentRows> {
        self.sheets.borrow().get(name).cloned()
    }

    /// Accepted saves so far.
    pub fn save_count(&self) -> usize {
        self.saves.get()
    }

    pub fn set_refusing(&self, refusing: bool) {
        self.refusing.set(refusing);
    }

    pub fn set_pending(&self, pending: bool) {
        self.pending.set(pending);
    }
}

impl SheetStore for MemorySheetStore {
    fn load(&self, name: &str) -> Loaded {
        if self.pending.get() {
            return Loaded::Pending;
        }
        match self.get(name) {
            Some(rows) => Loaded::Rows(rows),
            None => Loaded::Missing,
        }
    }

    fn save(&self, name: &str, rows: DocumentRows) -> bool {
        if self.refusing.get() {
            return false;
        }
        self.sheets.borrow_mut().insert(name.to_string(), rows);
        self.saves.set(self.saves.get() + 1);
        true
    }

    fn contains(&self, name: &str) -> bool {
        self.sheets.borrow().contains_key(name)
    }
}
```

- [ ] **Step 4: Write the failing grid tests**

`crates/geode-pricer/src/grid.rs`, tests first:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::sheet::tests::{at, callspread, line, push, result, spx};
    use crate::core::{Expansion, Sheet, Views};
    use geode_core::clock::Clock;
    use geode_core::pricing::OptionKind;

    /// [A, P(L1, L2), B].
    fn sheet() -> Sheet {
        let mut s = Sheet::new("t");
        push(&mut s, vec![line(spx(5000.0, OptionKind::Call), 1)]);
        push(&mut s, vec![callspread(1)]);
        push(&mut s, vec![line(spx(4000.0, OptionKind::Put), 1)]);
        s
    }

    fn plan() -> ColumnPlan {
        ColumnPlan::build(Views::builtin().get("vanilla").unwrap())
    }

    fn build(s: &Sheet, e: &Expansion, entry: Option<Place>) -> GridModel {
        GridModel::build(s, e, &plan(), entry, Clock::utc())
    }

    #[test]
    fn rows_follow_the_expansion_and_carry_depth_ids_and_the_tree_label() {
        let s = sheet();
        let closed = build(&s, &Expansion::default(), None);
        assert_eq!(closed.rows.len(), 3);
        assert_eq!(closed.rows[1].kind, GridRowKind::Package { open: false });
        assert_eq!(closed.rows[1].tree.as_ref(), "SPX Z26 4800/5200 CS");
        let mut e = Expansion::default();
        e.set(s.id(1), true);
        let open = build(&s, &e, None);
        assert_eq!(open.rows.len(), 5);
        assert_eq!(open.rows[1].kind, GridRowKind::Package { open: true });
        assert_eq!(open.rows[2].kind, GridRowKind::Leg);
        assert_eq!(open.rows[2].depth, 1);
        assert_eq!(open.rows[4].tree.as_ref(), "SPX Z26 4000 P");
        assert_eq!(open.grid_row_of(s.id(4)), Some(4));
        assert_eq!(open.columns.len(), plan().columns.len(), "the tree column is the delegate's own");
    }

    #[test]
    fn cells_carry_the_core_text_and_state() {
        let mut s = sheet();
        let answers: Vec<_> = (0..s.len())
            .filter(|r| s.is_line(*r))
            .map(|r| (s.id(r), s.revision(r), Ok(result(12.5))))
            .collect();
        s.deliver_all(answers, at(0));
        let m = build(&s, &Expansion::default(), None);
        let price = plan().columns.iter().position(|c| c.def.name == "price").unwrap();
        assert_eq!(m.rows[0].cells[price].text.as_ref(), "12.50");
        assert_eq!(m.rows[0].cells[price].state, CellState::Own);
        let strike = plan().columns.iter().position(|c| c.def.name == "strike").unwrap();
        assert_eq!(m.rows[1].cells[strike].state, CellState::Blank, "a package has no strike");
        assert!(m.columns[price].right && !m.columns[1].right, "numbers read down the right edge");
    }

    #[test]
    fn the_entry_placeholder_paints_where_its_place_lands_and_is_no_sheet_row() {
        let s = sheet();
        let mut e = Expansion::default();
        e.set(s.id(1), true);
        let m = build(&s, &e, Some(Place::Root { at: 1 }));
        assert_eq!(m.rows.len(), 6);
        assert_eq!(m.entry_row(), Some(1));
        assert_eq!(m.rows[1].kind, GridRowKind::Entry);
        assert_eq!(m.rows[1].id, None);
        let m = build(&s, &e, Some(Place::Leg { package: 1, leg: 2 }));
        assert_eq!(m.entry_row(), Some(4), "after the last leg");
        assert_eq!(m.rows[4].depth, 1);
        let m = build(&s, &e, Some(Place::Root { at: 5 }));
        assert_eq!(m.entry_row(), Some(5), "at the end");
        assert_eq!(s.len(), 5, "the sheet never holds the placeholder");
    }

    #[test]
    fn a_custom_package_labels_by_template_underlyings_and_expiries() {
        let mut s = Sheet::new("t");
        push(&mut s, vec![line(spx(5000.0, OptionKind::Call), 1)]);
        push(&mut s, vec![line(spx(4000.0, OptionKind::Put), 1)]);
        s.apply(crate::core::Edit::Group {
            first: 0,
            count: 2,
            template: crate::core::Template::Custom,
            id: None,
        })
        .unwrap();
        let m = build(&s, &Expansion::default(), None);
        assert_eq!(m.rows[0].tree.as_ref(), "CUSTOM SPX Z26");
    }
}
```

- [ ] **Step 5: Implement `grid.rs`**

Above the tests:

```rust
//! The table's prepared rows (line-pricer spec §8.2): every visible row's
//! cells as `SharedString`s with the core's `CellState`, built on every
//! edit, delivery, expansion, view, clock and entry change — never per
//! frame (`cell_text` allocates a `String` per cell). The paint is NOT
//! here: `Paints` resolves a state to a colour at render from a per-theme
//! memo (planning decision 14), so a theme switch rebuilds nothing.
//!
//! The entry placeholder (planning decision 11) is a row of this model
//! and never of the sheet.

use crate::core::columns::{CellState, ColumnKind, cell_text};
use crate::core::sheet::{LineId, Place, RowKind, Sheet};
use crate::core::shorthand::render_expiry;
use crate::core::tree::{Expansion, visible_rows};
use crate::core::views::ColumnPlan;
use geode_core::clock::Clock;
use gpui::SharedString;

#[derive(Debug, Clone)]
pub struct GridColumn {
    pub label: SharedString,
    /// Pixels (the vocabulary's widths; see `ColumnDef::default_width`).
    pub width: f32,
    /// Numbers read down the right edge.
    pub right: bool,
    pub kind: ColumnKind,
    pub editable: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GridRowKind {
    Line,
    Leg,
    Package { open: bool },
    /// The entry field's placeholder.
    Entry,
}

#[derive(Debug, Clone)]
pub struct GridCell {
    pub text: SharedString,
    pub state: CellState,
}

#[derive(Debug, Clone)]
pub struct GridRow {
    pub kind: GridRowKind,
    /// The sheet's flat row; `None` on the placeholder.
    pub row: Option<usize>,
    pub id: Option<LineId>,
    pub depth: usize,
    /// Column 0's label: the row's shorthand (spec §8.2).
    pub tree: SharedString,
    pub cells: Vec<GridCell>,
}

#[derive(Debug, Clone, Default)]
pub struct GridModel {
    pub columns: Vec<GridColumn>,
    pub rows: Vec<GridRow>,
}

fn right_aligned(kind: ColumnKind) -> bool {
    !matches!(
        kind,
        ColumnKind::Underlying
            | ColumnKind::Expiry
            | ColumnKind::Type
            | ColumnKind::BarrierType
            | ColumnKind::PricedAt
            | ColumnKind::Status
    )
}

/// A package's column-0 label: its template form while the legs still
/// match the table (the grammar round-trips it), else its template token
/// with its legs' distinct underlyings and expiries (spec §8.2).
fn package_label(sheet: &Sheet, row: usize) -> String {
    let text = sheet.shorthand(row);
    if !text.is_empty() && !text.contains('\n') {
        return text;
    }
    let RowKind::Package { template } = sheet.kind(row) else {
        return text;
    };
    let mut unds: Vec<String> = Vec::new();
    let mut exps: Vec<String> = Vec::new();
    for leg in sheet.children(row) {
        if let Some(i) = sheet.instrument(leg) {
            let u = i.underlying().to_string();
            if !unds.contains(&u) {
                unds.push(u);
            }
            let e = render_expiry(i.expiry());
            if !exps.contains(&e) {
                exps.push(e);
            }
        }
    }
    let mut parts = vec![template.token().to_string()];
    if !unds.is_empty() {
        parts.push(unds.join("/"));
    }
    if !exps.is_empty() {
        parts.push(exps.join("/"));
    }
    parts.join(" ")
}

/// The flat index a `Place` inserts at.
fn flat(place: Place) -> (usize, usize) {
    match place {
        Place::Root { at } => (at, 0),
        Place::Leg { package, leg } => (package + 1 + leg, 1),
    }
}

impl GridModel {
    pub fn build(
        sheet: &Sheet,
        expansion: &Expansion,
        plan: &ColumnPlan,
        entry: Option<Place>,
        clock: Clock,
    ) -> GridModel {
        let columns: Vec<GridColumn> = plan
            .columns
            .iter()
            .map(|c| GridColumn {
                label: c.label.clone().into(),
                width: c.width,
                right: right_aligned(c.def.kind),
                kind: c.def.kind,
                editable: c.def.editable,
            })
            .collect();
        let visible = visible_rows(sheet, expansion);
        let mut rows = Vec::with_capacity(visible.len() + usize::from(entry.is_some()));
        let placeholder = |depth: usize| GridRow {
            kind: GridRowKind::Entry,
            row: None,
            id: None,
            depth,
            tree: SharedString::default(),
            cells: plan
                .columns
                .iter()
                .map(|_| GridCell {
                    text: SharedString::default(),
                    state: CellState::Blank,
                })
                .collect(),
        };
        let mut pending = entry.map(flat);
        for r in visible {
            if let Some((at, depth)) = pending
                && r >= at
            {
                rows.push(placeholder(depth));
                pending = None;
            }
            let kind = match sheet.kind(r) {
                RowKind::Package { .. } => GridRowKind::Package {
                    open: expansion.is_open(sheet.id(r)),
                },
                RowKind::Line | RowKind::Underlying if sheet.parent(r).is_some() => GridRowKind::Leg,
                RowKind::Line | RowKind::Underlying => GridRowKind::Line,
            };
            let tree = if sheet.is_package(r) {
                package_label(sheet, r)
            } else {
                sheet.shorthand(r)
            };
            rows.push(GridRow {
                kind,
                row: Some(r),
                id: Some(sheet.id(r)),
                depth: sheet.depth(r),
                tree: tree.into(),
                cells: plan
                    .columns
                    .iter()
                    .map(|c| {
                        let t = cell_text(sheet, r, c.def, &c.format, clock);
                        GridCell {
                            text: t.text.into(),
                            state: t.state,
                        }
                    })
                    .collect(),
            });
        }
        if let Some((_, depth)) = pending {
            rows.push(placeholder(depth));
        }
        GridModel { columns, rows }
    }

    pub fn grid_row_of(&self, id: LineId) -> Option<usize> {
        self.rows.iter().position(|r| r.id == Some(id))
    }

    pub fn entry_row(&self) -> Option<usize> {
        self.rows.iter().position(|r| r.kind == GridRowKind::Entry)
    }
}
```

If `Template::token` is not `pub` (it is, `template.rs:75`), or `c.label` is already a `SharedString`, adjust the conversions only.

- [ ] **Step 6: Write the failing paint sweep**

`crates/geode-pricer/src/paint.rs`, tests first:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use geode_core::colour::{READABLE_RATIO, contrast_ratio};
    use gpui_component::{ActiveTheme as _, Theme};

    /// Spec §8.2: every paint the pricer adds, swept over every bundled
    /// theme with NO exception list — each text colour against the ground
    /// it actually paints on (the table over the window background for a
    /// line row, `secondary` over that for a package row).
    #[gpui::test]
    fn every_pricer_paint_is_readable_on_every_bundled_theme(cx: &mut gpui::TestAppContext) {
        cx.update(gpui_component::init);
        let (service, _) = geode_shell::theme::load_bundled();
        let mut failures = Vec::new();
        let mut checked = 0;
        for name in service.names() {
            let entry = service.resolve(&name).unwrap().clone();
            cx.update(|cx| {
                Theme::global_mut(cx).apply_config(&entry);
                let theme = cx.theme();
                let p = Paints::derive(theme);
                let ground = over(theme.table, to_rgb(theme.background));
                let package = to_rgb(p.package_ground);
                for (label, text, bg) in [
                    ("own", p.own, ground),
                    ("muted", p.muted, ground),
                    ("danger", p.danger, ground),
                    ("package own", p.package_own, package),
                    ("package muted", p.package_muted, package),
                    ("package danger", p.package_danger, package),
                ] {
                    checked += 1;
                    let ratio = contrast_ratio(to_rgb(text), bg);
                    if ratio < READABLE_RATIO {
                        failures.push(format!("{name}: {label} at {ratio:.2}:1"));
                    }
                }
            });
        }
        assert!(checked >= 6 * 40, "every bundled theme was swept ({checked})");
        assert!(failures.is_empty(), "unreadable pricer paints:\n{}", failures.join("\n"));
    }

    #[gpui::test]
    fn a_state_picks_its_colour_and_a_package_row_its_own(cx: &mut gpui::TestAppContext) {
        cx.update(gpui_component::init);
        cx.update(|cx| {
            let p = Paints::derive(cx.theme());
            assert_eq!(p.text(CellState::Own, false), p.own);
            assert_eq!(p.text(CellState::Stale, false), p.muted);
            assert_eq!(p.text(CellState::Inherited, false), p.muted);
            assert_eq!(p.text(CellState::Failed, false), p.danger);
            assert_eq!(p.text(CellState::Own, true), p.package_own);
            assert_eq!(p.text(CellState::Failed, true), p.package_danger);
        });
    }
}
```

If fewer than 40 themes are bundled when this runs, lower the `checked` floor to `6 * <count>` and say so in the commit — the point is that the sweep ran over all of them.

- [ ] **Step 7: Implement `paint.rs`**

```rust
//! The pricer's text colours (line-pricer spec §8.2), derived once per
//! theme and floored to `READABLE_RATIO` against the ground each paints
//! on (planning decision 14): an own value in `foreground`, a stale
//! result or an inherited shift `muted`, a failed row's cells in
//! `Tone::DangerText`, and a package row on `secondary` with its own
//! floored trio. Resolved in `render_td` from the cell's `CellState`; the
//! `GridModel` stays theme-free. The tile re-derives it when the theme
//! global changes (an observer, never a per-cell check).

use crate::core::columns::CellState;
use geode_core::colour::{Rgb, readable_on};
use geode_shell::shell::chip::{Tone, chip_paint};
use geode_shell::shell::colours::{over, to_hsla, to_rgb};
use gpui::Hsla;
use gpui_component::Theme;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Paints {
    pub own: Hsla,
    pub muted: Hsla,
    pub danger: Hsla,
    /// Opaque: `secondary` composited over the table ground.
    pub package_ground: Hsla,
    pub package_own: Hsla,
    pub package_muted: Hsla,
    pub package_danger: Hsla,
}

impl Paints {
    pub fn derive(theme: &Theme) -> Paints {
        let ground: Rgb = over(theme.table, to_rgb(theme.background));
        let package: Rgb = over(theme.secondary, ground);
        let toward = to_rgb(theme.foreground);
        let floor = |c: Hsla, bg: Rgb| to_hsla(readable_on(to_rgb(c), bg, toward));
        let danger = chip_paint(theme, Tone::DangerText).text;
        Paints {
            own: floor(theme.foreground, ground),
            muted: floor(theme.muted_foreground, ground),
            danger: floor(danger, ground),
            package_ground: to_hsla(package),
            package_own: floor(theme.foreground, package),
            package_muted: floor(theme.muted_foreground, package),
            package_danger: floor(danger, package),
        }
    }

    pub fn text(&self, state: CellState, package: bool) -> Hsla {
        match (state, package) {
            (CellState::Stale | CellState::Inherited, false) => self.muted,
            (CellState::Stale | CellState::Inherited, true) => self.package_muted,
            (CellState::Failed, false) => self.danger,
            (CellState::Failed, true) => self.package_danger,
            (CellState::Own | CellState::Blank, false) => self.own,
            (CellState::Own | CellState::Blank, true) => self.package_own,
        }
    }
}
```

If `over`'s argument order differs from `shell/colours.rs:55,143`, follow the file. If the sweep fails on a theme, the floor is wrong or the ground is wrong — never add an exception; report the theme and ratio.

- [ ] **Step 8: Run the new tests**

Run: `cargo test -p geode-pricer store grid paint`
Expected: PASS.

- [ ] **Step 9: Add the grid bench**

In `crates/geode-pricer/benches/core.rs`, extend the imports:

```rust
use geode_core::clock::Clock;
use geode_pricer::core::{ColumnPlan, Expansion, Views};
use geode_pricer::grid::GridModel;
```

and before `g.finish();`:

```rust
    // Spec §8.2 / §12: the grid model is rebuilt on every edit, delivery
    // and expansion change, so a whole build at 1,000 lines — every
    // package open, every line answered — is the per-keystroke cost the
    // 8 ms budget constrains.
    let mut s = sheet(1_000);
    let answers: Vec<(LineId, u64, Result<PriceResult, String>)> = (0..s.len())
        .filter(|r| s.is_line(*r))
        .map(|r| (s.id(r), s.revision(r), Ok(PriceResult { price: 12.5, delta: 0.5, gamma: 0.01, vega: 1.0, theta: -0.5, rho: 0.1 })))
        .collect();
    s.deliver_all(answers, Utc::now());
    let mut expansion = Expansion::default();
    expansion.open_all(&s);
    let views = Views::builtin();
    let plan = ColumnPlan::build(views.get("vanilla").expect("bundled"));
    g.bench_function("grid_build_1000", |b| {
        b.iter(|| black_box(GridModel::build(&s, &expansion, &plan, None, Clock::utc())))
    });
```

Update the file's module doc to name the grid build. Run: `cargo bench -p geode-pricer -- grid_build_1000` once and note the median for Task 14 (it is expected well under 8 ms; if it is not, stop and report before going on).

- [ ] **Step 10: Gate and commit**

Run: `cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`

```bash
git add crates/geode-pricer/
git commit -m "feat(pricer): sheet store seam, prepared grid model, floored paints, grid bench

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---
### Task 6: The hosted tile — factory, content, keymap, delegate, header, session record, restore and naming

The tile appears, restores a named sheet from the store, paints it, answers flip barriers, follows the app clock and `:view`, and takes a reloaded views doc. Navigation, pricing, entry, editing and the structural verbs are Tasks 7–12; this task registers every action and binds the whole fragment now, so later tasks only add dispatch arms.

**Files:**
- Create: `crates/geode-pricer/src/session.rs`, `src/content.rs`, `src/delegate.rs`, `src/header.rs`, `src/tile.rs`
- Modify: `crates/geode-pricer/src/lib.rs`
- Test: `session.rs`, `header.rs`, `content.rs` (`mod tests`, plain), `tile.rs` (`mod tests`, `#[gpui::test]`)

**Interfaces:**
- Consumes: Task 3's `Expansion`, Task 4's `commands`, Task 5's `SheetStore`/`Loaded`/`MemorySheetStore`/`GridModel`/`Paints`, `core::{Sheet, Views, ColumnPlan, from_rows, Refresh, LineId, storage::{encode_refresh, parse_refresh}}`, `geode_shell::module::{TileContent, ModuleFactory, TileOccupant, Delivery, FindEvent, StackHandle}`, `geode_shell::keymap::KeyContext`, `geode_shell::actions::{ActionDef, ActionId, ActionRegistry}`, `geode_shell::frame::Frame`, `geode_shell::clock::AppClock`.
- Produces (later tasks build on exactly these names):

```rust
// session.rs
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Record { pub sheet: Option<String>, pub view: Option<String>, pub refresh: Option<Refresh>,
                    pub cursor: Option<LineId>, pub expanded: Vec<LineId> }
impl Record { pub fn from_table(t: &toml::Table) -> Record; pub fn to_table(&self) -> toml::Table; }

// content.rs
pub const ACTIONS: &[(&str, &str)];
pub const NO_DEFAULT_KEY: &[&str];
pub const DEFAULT_KEYMAP: &str;
#[derive(Debug, Clone, PartialEq)]
pub struct PricerSettings { pub pricer: String, pub pricer_missing: bool, pub refresh: Option<Duration>, pub stale_after: Duration }
pub(crate) struct Shared { pub(crate) views: RefCell<Views>, pub(crate) settings: RefCell<PricerSettings>,
    pub(crate) store: Rc<dyn SheetStore>, pub(crate) open: RefCell<BTreeSet<String>>,
    pub(crate) tiles: RefCell<Vec<WeakEntity<PricerTile>>> }
pub struct PricerFactory { /* data, shared */ }
impl PricerFactory {
    pub fn new(data: DataHandle, store: Rc<dyn SheetStore>, views: Views, settings: PricerSettings) -> Self;
    pub fn reload(&self, views: Views, refresh: Option<Duration>, stale_after: Duration, cx: &mut App);
    pub fn view_names(&self) -> Vec<String>;
    pub fn settings(&self) -> PricerSettings;
}

// tile.rs (pub(crate) unless noted)
pub struct PricerTile { .. }
impl PricerTile {
    pub fn new(id: TileId, frame: Entity<Frame>, data: DataHandle, shared: Rc<Shared>,
               restored: Option<&toml::Table>, window: &mut Window, cx: &mut Context<Self>) -> Self;
    pub fn key_context(&self) -> KeyContext;
    pub fn holds_focus(&self, window: &Window, cx: &App) -> bool;
    pub fn title(&self) -> SharedString;
    pub fn serialize(&self) -> toml::Table;
    pub fn set_stack(&mut self, stack: Option<StackHandle>, cx: &mut Context<Self>);
    pub fn set_visible(&mut self, visible: bool, cx: &mut Context<Self>);
    pub fn find(&mut self, event: FindEvent, window: &mut Window, cx: &mut Context<Self>);
    pub fn dispatch(&mut self, action: &ActionId, count: Option<u32>, window: &mut Window, cx: &mut Context<Self>) -> bool;
    pub fn command(&mut self, line: &str, window: &mut Window, cx: &mut Context<Self>) -> Result<(), String>;
    pub fn completions(&self, line: &str, cursor: usize) -> Vec<String>;
    pub fn deliver(&mut self, outcome: PriceOutcome, cx: &mut Context<Self>);     // Task 8 fills it
    pub fn loaded(&mut self, answer: Result<Option<DocumentRows>, String>, cx: &mut Context<Self>);
    pub(crate) fn config_changed(&mut self, cx: &mut Context<Self>);
    fn rebuild(&mut self, cx: &mut Context<Self>);          // model + install + chrome + notify
    fn install_model(&mut self, cx: &mut Context<Self>);
    fn sync_cursor(&mut self, cx: &mut Context<Self>);
    fn resolve_plan(&mut self);
}
pub(crate) struct Cursor { pub line: Option<LineId>, pub col: usize, pub last_row: usize }

// delegate.rs
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChevronClicked(pub usize);
pub struct SheetDelegate { pub(crate) model: Rc<GridModel>, pub(crate) cursor: Option<(usize, usize)>, pub(crate) paints: Paints, .. }
pub(crate) const TREE_COL: usize = 0;

// header.rs
pub(crate) struct HeaderInputs<'a> { pub sheet: &'a Sheet, pub notice: Option<SharedString>, pub settings: &'a PricerSettings, pub clock: Clock }
pub(crate) struct HeaderModel { pub name, pub view, pub shifts: Vec<SharedString>, pub pricer, pub pricing: Option<SharedString>,
    pub last_priced: Option<DateTime<Utc>>, pub time: Option<SharedString>, pub time_stale: Option<SharedString>,
    pub notice: Option<SharedString>, pub stale: bool }
pub(crate) fn prepare(i: HeaderInputs) -> HeaderModel;
pub(crate) fn render(h: &HeaderModel, theme: &Theme, stack: Option<&StackHandle>, tile: TileId) -> impl IntoElement;
pub(crate) fn render_footer(text: Option<&SharedString>, theme: &Theme) -> impl IntoElement;
```

- [ ] **Step 1: The session record, test first**

`crates/geode-pricer/src/session.rs`:

```rust
//! The tile's session record (line-pricer spec §7.4): `{ sheet, view,
//! refresh, cursor, expanded }`. The sheet's rows are the store's; this
//! names which sheet the tile shows and how it was looking at it. Read
//! leniently — a key of the wrong type is ignored, never a refusal — so
//! a hand-edited `session.toml` still opens the tile.

use crate::core::sheet::{LineId, Refresh};
use crate::core::storage::{encode_refresh, parse_refresh};

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Record {
    pub sheet: Option<String>,
    pub view: Option<String>,
    pub refresh: Option<Refresh>,
    pub cursor: Option<LineId>,
    pub expanded: Vec<LineId>,
}

fn id(v: &toml::Value) -> Option<LineId> {
    v.as_integer().and_then(|i| u64::try_from(i).ok()).map(LineId)
}

impl Record {
    pub fn from_table(t: &toml::Table) -> Record {
        Record {
            sheet: t.get("sheet").and_then(|v| v.as_str()).map(str::to_string),
            view: t.get("view").and_then(|v| v.as_str()).map(str::to_string),
            refresh: t
                .get("refresh")
                .and_then(|v| v.as_str())
                .and_then(|s| parse_refresh(s).ok()),
            cursor: t.get("cursor").and_then(id),
            expanded: t
                .get("expanded")
                .and_then(|v| v.as_array())
                .map(|a| a.iter().filter_map(id).collect())
                .unwrap_or_default(),
        }
    }

    pub fn to_table(&self) -> toml::Table {
        let mut t = toml::Table::new();
        if let Some(s) = &self.sheet {
            t.insert("sheet".into(), s.clone().into());
        }
        if let Some(v) = &self.view {
            t.insert("view".into(), v.clone().into());
        }
        if let Some(r) = self.refresh {
            t.insert("refresh".into(), encode_refresh(r).into());
        }
        if let Some(c) = self.cursor {
            t.insert("cursor".into(), toml::Value::Integer(c.0 as i64));
        }
        if !self.expanded.is_empty() {
            t.insert(
                "expanded".into(),
                toml::Value::Array(
                    self.expanded
                        .iter()
                        .map(|id| toml::Value::Integer(id.0 as i64))
                        .collect(),
                ),
            );
        }
        t
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn a_record_round_trips_through_its_table() {
        let r = Record {
            sheet: Some("book".into()),
            view: Some("barrier".into()),
            refresh: Some(Refresh::Every(Duration::from_secs(10))),
            cursor: Some(LineId(7)),
            expanded: vec![LineId(2), LineId(9)],
        };
        assert_eq!(Record::from_table(&r.to_table()), r);
        assert_eq!(Record::from_table(&Record::default().to_table()), Record::default());
    }

    #[test]
    fn a_wrong_typed_key_is_ignored_not_refused() {
        let t: toml::Table =
            toml::from_str("sheet = 3\ncursor = -1\nexpanded = [\"x\", 4]\nrefresh = \"soon\"").unwrap();
        assert_eq!(
            Record::from_table(&t),
            Record { expanded: vec![LineId(4)], ..Record::default() }
        );
    }
}
```

`encode_refresh`/`parse_refresh` are `pub` in `crate::core::storage` (Part 2). Add `pub mod session;` to `lib.rs`. Run `cargo test -p geode-pricer session` — PASS.

- [ ] **Step 2: The header model, test first**

`crates/geode-pricer/src/header.rs` — the pure `prepare` and its tests now; the two render functions in Step 5:

```rust
//! The tile's one dense header row and its footer (line-pricer spec §8.3).
//! `prepare` formats everything once per change; `render` paints the
//! prepared strings and compares the last priced time against
//! `stale_after` (a compare, never a format, per frame).

use crate::content::PricerSettings;
use crate::core::sheet::Sheet;
use chrono::{DateTime, Utc};
use geode_core::clock::Clock;
use gpui::SharedString;

pub(crate) const HEADER_HEIGHT: f32 = 22.0;
pub(crate) const FOOTER_HEIGHT: f32 = 20.0;

pub(crate) struct HeaderInputs<'a> {
    pub sheet: &'a Sheet,
    /// The tile's own notice, already chosen by precedence (a transient
    /// notice, then a view fallback); `None` lets a missing pricer speak.
    pub notice: Option<SharedString>,
    pub settings: &'a PricerSettings,
    pub clock: Clock,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct HeaderModel {
    pub name: SharedString,
    pub view: SharedString,
    /// `spot +2%`, `vol −1`: the sheet-wide shifts, only those set.
    pub shifts: Vec<SharedString>,
    pub pricer: SharedString,
    /// `N pricing…` while any line is stale.
    pub pricing: Option<SharedString>,
    pub last_priced: Option<DateTime<Utc>>,
    pub time: Option<SharedString>,
    pub time_stale: Option<SharedString>,
    pub notice: Option<SharedString>,
    /// Set in `render` from `last_priced` and `stale_after`.
    pub stale: bool,
}

/// `+2` / `−1.5`: a shift is a delta, so its sign is the information; the
/// minus is U+2212 (spec §8.3's `vol −1`).
fn signed(v: f64) -> String {
    if v < 0.0 {
        format!("\u{2212}{}", -v)
    } else {
        format!("+{v}")
    }
}

pub(crate) fn prepare(i: HeaderInputs) -> HeaderModel {
    let s = i.sheet;
    let own = s.sheet_shift();
    let mut shifts = Vec::new();
    if let Some(v) = own.spot_pct {
        shifts.push(format!("spot {}%", signed(v)).into());
    }
    if let Some(v) = own.vol_pts {
        shifts.push(format!("vol {}", signed(v)).into());
    }
    let stale = s.stale_lines().count();
    let last_priced = (0..s.len())
        .filter(|r| s.is_line(*r))
        .filter_map(|r| s.priced_at(r))
        .max();
    let time = last_priced.map(|t| i.clock.hms(t));
    let notice = i.notice.or_else(|| {
        i.settings
            .pricer_missing
            .then(|| format!("pricer \"{}\" is not built into this binary", i.settings.pricer).into())
    });
    HeaderModel {
        name: s.name.clone().into(),
        view: s.view.clone().into(),
        shifts,
        pricer: i.settings.pricer.clone().into(),
        pricing: (stale > 0).then(|| format!("{stale} pricing…").into()),
        last_priced,
        time_stale: time.as_ref().map(|t| format!("{t} stale").into()),
        time: time.map(Into::into),
        notice,
        stale: false,
    }
}

impl HeaderModel {
    /// Everything the header paints, for tests.
    #[cfg(test)]
    pub(crate) fn texts(&self) -> Vec<String> {
        let mut out = vec![self.name.to_string(), self.view.to_string()];
        out.extend(self.shifts.iter().map(|s| s.to_string()));
        out.extend(self.notice.iter().map(|s| s.to_string()));
        out.extend(self.pricing.iter().map(|s| s.to_string()));
        out.push(self.pricer.to_string());
        out.extend(self.time.iter().map(|s| s.to_string()));
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::edit::Edit;
    use crate::core::sheet::OwnShifts;
    use crate::core::sheet::tests::{at, line, push, result, spx};
    use geode_core::pricing::OptionKind;

    fn settings(missing: bool) -> PricerSettings {
        PricerSettings {
            pricer: "vendor".into(),
            pricer_missing: missing,
            ..PricerSettings::default()
        }
    }

    #[test]
    fn the_header_names_the_sheet_view_shifts_and_pricing_count() {
        let mut s = Sheet::new("book");
        push(&mut s, vec![line(spx(5000.0, OptionKind::Call), 1)]);
        push(&mut s, vec![line(spx(4000.0, OptionKind::Put), 1)]);
        s.apply(Edit::SetSheetShift(OwnShifts { spot_pct: Some(2.0), vol_pts: Some(-1.0) }))
            .unwrap();
        let h = prepare(HeaderInputs { sheet: &s, notice: None, settings: &settings(false), clock: Clock::utc() });
        assert_eq!(
            h.texts(),
            vec!["book", "vanilla", "spot +2%", "vol \u{2212}1", "2 pricing…", "vendor"]
        );
        let id = s.id(0);
        let rev = s.revision(0);
        s.deliver(id, rev, Ok(result(1.0)), at(0));
        let h = prepare(HeaderInputs { sheet: &s, notice: None, settings: &settings(false), clock: Clock::utc() });
        assert_eq!(h.pricing.as_deref(), Some("1 pricing…"));
        assert_eq!(h.last_priced, Some(at(0)));
        assert!(h.time.is_some());
    }

    #[test]
    fn a_missing_pricer_speaks_only_when_nothing_else_does() {
        let s = Sheet::new("book");
        let h = prepare(HeaderInputs { sheet: &s, notice: None, settings: &settings(true), clock: Clock::utc() });
        assert_eq!(h.notice.as_deref(), Some("pricer \"vendor\" is not built into this binary"));
        let h = prepare(HeaderInputs {
            sheet: &s,
            notice: Some("loading…".into()),
            settings: &settings(true),
            clock: Clock::utc(),
        });
        assert_eq!(h.notice.as_deref(), Some("loading…"));
    }
}
```

`PricerSettings` is defined in Step 3; write Step 3's `content.rs` settings block before running these.

- [ ] **Step 3: `content.rs` — actions, keymap, settings, factory, content**

```rust
//! What the shell hosts (line-pricer spec §8.1): the [`TileContent`]
//! wrapper over a [`PricerTile`], and the factory that builds them. The
//! factory carries the data handle, the loaded views, the pricing
//! settings, the sheet store and the set of sheet names open across its
//! tiles (spec §7.4) — the shell sees none of them.

use crate::core::views::Views;
use crate::store::SheetStore;
use crate::tile::PricerTile;
use geode_data::DataHandle;
use geode_shell::actions::{ActionDef, ActionId, ActionRegistry};
use geode_shell::diagnostics::Diagnostics;
use geode_shell::frame::Frame;
use geode_shell::keymap::KeyContext;
use geode_shell::module::{Delivery, FindEvent, ModuleFactory, StackHandle, TileContent, TileOccupant};
use geode_shell::tiling::TileId;
use gpui::prelude::*;
use gpui::{App, Entity, SharedString, WeakEntity, Window};
use std::cell::RefCell;
use std::collections::BTreeSet;
use std::rc::Rc;
use std::time::Duration;

/// Every action this module registers, with its palette title — one list
/// that [`DEFAULT_KEYMAP`] binds and `register_actions` registers.
pub const ACTIONS: &[(&str, &str)] = &[
    ("pricer::down", "Cursor down"),
    ("pricer::up", "Cursor up"),
    ("pricer::left", "Cursor left"),
    ("pricer::right", "Cursor right"),
    ("pricer::top", "First row"),
    ("pricer::bottom", "Last row"),
    ("pricer::first_col", "First column"),
    ("pricer::last_col", "Last column"),
    ("pricer::page_down", "Half page down"),
    ("pricer::page_up", "Half page up"),
    ("pricer::page_down_full", "Page down"),
    ("pricer::page_up_full", "Page up"),
    ("pricer::yank_row", "Yank row"),
    ("pricer::yank_col", "Yank column"),
    ("pricer::find_next", "Find next"),
    ("pricer::find_prev", "Find previous"),
    ("pricer::escape", "Clear the footer"),
    ("pricer::add_below", "Add a line below"),
    ("pricer::add_above", "Add a line above"),
    ("pricer::edit", "Edit cell"),
    ("pricer::delete", "Delete row"),
    ("pricer::undo", "Undo"),
    ("pricer::redo", "Redo"),
    ("pricer::put_below", "Put below"),
    ("pricer::put_above", "Put above"),
    ("pricer::move_down", "Move row down"),
    ("pricer::move_up", "Move row up"),
    ("pricer::group", "Group into a package"),
    ("pricer::ungroup", "Ungroup package"),
    ("pricer::menu", "Pricer actions…"),
    ("pricer::toggle", "Toggle package"),
    ("pricer::expand", "Expand package"),
    ("pricer::collapse", "Collapse package"),
    ("pricer::expand_all", "Expand all packages"),
    ("pricer::collapse_all", "Collapse all packages"),
    ("pricer::price", "Reprice every line"),
    ("pricer::commit", "Commit"),
    ("pricer::cancel", "Cancel"),
    ("pricer::insert_up", "Up"),
    ("pricer::insert_down", "Down"),
    ("pricer::insert_up_big", "Up ten"),
    ("pricer::insert_down_big", "Down ten"),
    ("pricer::menu_down", "Menu: down"),
    ("pricer::menu_up", "Menu: up"),
    ("pricer::menu_pick", "Menu: pick"),
    ("pricer::menu_close", "Menu: close"),
];

/// Registered but deliberately unbound: `:price` and the menu reach it.
pub const NO_DEFAULT_KEY: &[&str] = &["pricer::price"];

/// The module's keymap fragment (spec §8.4–§8.5). Every predicate is a
/// plain conjunction whose first identifier is `pricer`. No chord is bound
/// in `insert` or `entry` mode, so `ctrl+k` keeps opening the palette
/// from inside a field. `y` alone is NOT bound (planning decision 16): an
/// exact match dispatches at once, so it would make `y y` and `y c`
/// unreachable. `g` alone is not bound for the same reason (`g g`, `g p`,
/// `g u`).
pub const DEFAULT_KEYMAP: &str = r#"
[[bindings]]
context = "pricer && mode == normal"
[bindings.keys]
"j" = "pricer::down"
"k" = "pricer::up"
"h" = "pricer::left"
"l" = "pricer::right"
"down" = "pricer::down"
"up" = "pricer::up"
"left" = "pricer::left"
"right" = "pricer::right"
"g g" = "pricer::top"
"shift+g" = "pricer::bottom"
"^" = "pricer::first_col"
"$" = "pricer::last_col"
"home" = "pricer::first_col"
"end" = "pricer::last_col"
"ctrl+d" = "pricer::page_down"
"ctrl+u" = "pricer::page_up"
"ctrl+f" = "pricer::page_down_full"
"ctrl+b" = "pricer::page_up_full"
"pagedown" = "pricer::page_down_full"
"pageup" = "pricer::page_up_full"
"y y" = "pricer::yank_row"
"y c" = "pricer::yank_col"
"n" = "pricer::find_next"
"shift+n" = "pricer::find_prev"
"escape" = "pricer::escape"
"o" = "pricer::add_below"
"shift+o" = "pricer::add_above"
"i" = "pricer::edit"
"enter" = "pricer::edit"
"d d" = "pricer::delete"
"u" = "pricer::undo"
"ctrl+r" = "pricer::redo"
"p" = "pricer::put_below"
"shift+p" = "pricer::put_above"
"shift+j" = "pricer::move_down"
"shift+k" = "pricer::move_up"
"g p" = "pricer::group"
"g u" = "pricer::ungroup"
"." = "pricer::menu"
"space" = "pricer::toggle"
"z a" = "pricer::toggle"
"z o" = "pricer::expand"
"z c" = "pricer::collapse"
"z shift+r" = "pricer::expand_all"
"z shift+m" = "pricer::collapse_all"

[[bindings]]
context = "pricer && mode == insert"
[bindings.keys]
"enter" = "pricer::commit"
"escape" = "pricer::cancel"
"up" = "pricer::insert_up"
"down" = "pricer::insert_down"
"shift+up" = "pricer::insert_up_big"
"shift+down" = "pricer::insert_down_big"

[[bindings]]
context = "pricer && mode == entry"
[bindings.keys]
"enter" = "pricer::commit"
"escape" = "pricer::cancel"
"up" = "pricer::insert_up"
"down" = "pricer::insert_down"

[[bindings]]
context = "pricer && mode == menu"
[bindings.keys]
"j" = "pricer::menu_down"
"k" = "pricer::menu_up"
"down" = "pricer::menu_down"
"up" = "pricer::menu_up"
"enter" = "pricer::menu_pick"
"escape" = "pricer::menu_close"
"." = "pricer::menu_close"
"#;

/// The app-level settings every tile reads (spec §5.5, §8.3, §9.4):
/// the pricer's name for the header and whether this binary has it, the
/// `[pricing] refresh` default (`None` = off), and the shell's
/// `stale_after`.
#[derive(Debug, Clone, PartialEq)]
pub struct PricerSettings {
    pub pricer: String,
    pub pricer_missing: bool,
    pub refresh: Option<Duration>,
    pub stale_after: Duration,
}

impl Default for PricerSettings {
    fn default() -> Self {
        PricerSettings {
            pricer: "mock".into(),
            pricer_missing: false,
            refresh: Some(Duration::from_secs(30)),
            stale_after: Duration::from_secs(15 * 60),
        }
    }
}

/// What the factory shares with every tile it built.
pub(crate) struct Shared {
    pub(crate) views: RefCell<Views>,
    pub(crate) settings: RefCell<PricerSettings>,
    pub(crate) store: Rc<dyn SheetStore>,
    /// Sheet names open in some tile (spec §7.4): `untitled-N` skips them,
    /// and Part 4's `:e` refuses them, so two writers never race.
    pub(crate) open: RefCell<BTreeSet<String>>,
    /// So a reload reaches every open tile (planning decision 20).
    pub(crate) tiles: RefCell<Vec<WeakEntity<PricerTile>>>,
}

pub struct PricerContent {
    tile: Entity<PricerTile>,
}

impl TileContent for PricerContent {
    fn key_context(&self, cx: &App) -> KeyContext {
        self.tile.read(cx).key_context()
    }

    fn dispatch(&self, action: &ActionId, count: Option<u32>, window: &mut Window, cx: &mut App) -> bool {
        self.tile.update(cx, |t, cx| t.dispatch(action, count, window, cx))
    }

    fn command(&self, line: &str, window: &mut Window, cx: &mut App) -> Result<(), String> {
        self.tile.update(cx, |t, cx| t.command(line, window, cx))
    }

    fn completions(&self, line: &str, cursor: usize, cx: &App) -> Vec<String> {
        self.tile.read(cx).completions(line, cursor)
    }

    fn find(&self, event: FindEvent, window: &mut Window, cx: &mut App) {
        self.tile.update(cx, |t, cx| t.find(event, window, cx))
    }

    fn deliver(&self, delivery: Delivery, _window: &mut Window, cx: &mut App) {
        match delivery {
            Delivery::Price(outcome) => self.tile.update(cx, |t, cx| t.deliver(outcome, cx)),
            // Part 4's store answers a sheet load here; until then this
            // tile asks no document or view query and fetches no series,
            // so any of these is a routing bug.
            Delivery::Query(_) => {}
            Delivery::Series(_) | Delivery::SeriesFetched { .. } => {}
        }
    }

    fn set_visible(&self, visible: bool, cx: &mut App) {
        self.tile.update(cx, |t, cx| t.set_visible(visible, cx))
    }

    fn set_stack(&self, stack: Option<StackHandle>, cx: &mut App) {
        self.tile.update(cx, |t, cx| t.set_stack(stack, cx))
    }

    fn title(&self, cx: &App) -> SharedString {
        self.tile.read(cx).title()
    }

    fn serialize(&self, cx: &App) -> toml::Table {
        self.tile.read(cx).serialize()
    }

    fn holds_focus(&self, window: &Window, cx: &App) -> bool {
        self.tile.read(cx).holds_focus(window, cx)
    }
}

pub struct PricerFactory {
    data: DataHandle,
    shared: Rc<Shared>,
}

impl PricerFactory {
    pub fn new(data: DataHandle, store: Rc<dyn SheetStore>, views: Views, settings: PricerSettings) -> Self {
        PricerFactory {
            data,
            shared: Rc::new(Shared {
                views: RefCell::new(views),
                settings: RefCell::new(settings),
                store,
                open: RefCell::new(BTreeSet::new()),
                tiles: RefCell::new(Vec::new()),
            }),
        }
    }

    /// A reload (planning decision 20): the new views and the live pricing
    /// settings, then every open tile re-resolves its view and restarts
    /// its timer. The pricer's name and presence are the running data
    /// engine's and change only with a restart (`[pricing] adapter`).
    pub fn reload(&self, views: Views, refresh: Option<Duration>, stale_after: Duration, cx: &mut App) {
        *self.shared.views.borrow_mut() = views;
        {
            let mut s = self.shared.settings.borrow_mut();
            s.refresh = refresh;
            s.stale_after = stale_after;
        }
        let tiles: Vec<WeakEntity<PricerTile>> = {
            let mut t = self.shared.tiles.borrow_mut();
            t.retain(|w| w.upgrade().is_some());
            t.clone()
        };
        for tile in tiles {
            if let Some(tile) = tile.upgrade() {
                tile.update(cx, |t, cx| t.config_changed(cx));
            }
        }
    }

    pub fn view_names(&self) -> Vec<String> {
        self.shared.views.borrow().names().map(str::to_string).collect()
    }

    pub fn settings(&self) -> PricerSettings {
        self.shared.settings.borrow().clone()
    }
}

impl ModuleFactory for PricerFactory {
    fn kind(&self) -> &'static str {
        "pricer"
    }

    fn register_actions(&self, registry: &mut ActionRegistry) {
        for (id, title) in ACTIONS {
            let _ = registry.register(ActionDef {
                id: ActionId((*id).to_string()),
                title: (*title).to_string(),
                category: "Pricer".to_string(),
            });
        }
    }

    fn default_keymap(&self) -> Option<&'static str> {
        Some(DEFAULT_KEYMAP)
    }

    fn create(
        &self,
        tile: TileId,
        restored: Option<&toml::Table>,
        frame: Entity<Frame>,
        diagnostics: Entity<Diagnostics>,
        window: &mut Window,
        cx: &mut App,
    ) -> TileOccupant {
        // The pricer reads no catalogue in Part 3 (planning decision 17).
        let _ = diagnostics;
        let entity = cx.new(|cx| {
            PricerTile::new(tile, frame, self.data.clone(), self.shared.clone(), restored, window, cx)
        });
        self.shared.tiles.borrow_mut().push(entity.downgrade());
        TileOccupant {
            kind: self.kind(),
            view: entity.clone().into(),
            content: Box::new(PricerContent { tile: entity }),
        }
    }
}
```

Tests for the fragment go in `content.rs` (window-free, the market-data pattern at `geode-marketdata/src/content.rs:376-680`):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use geode_shell::defaults::default_mod;
    use geode_shell::keymap::fragments::{check_fragment, fragment_doc};
    use geode_shell::keymap::{KeyContext, MatchResult, Matcher, build_keymap, parse_keystroke};

    fn registry() -> ActionRegistry {
        let mut r = ActionRegistry::default();
        for (id, title) in ACTIONS {
            r.register(ActionDef { id: ActionId(id.to_string()), title: title.to_string(), category: "Pricer".into() })
                .unwrap();
        }
        r
    }

    fn keymap() -> geode_shell::keymap::Keymap {
        let doc = fragment_doc("pricer", DEFAULT_KEYMAP).unwrap();
        let (doc, diags) = check_fragment(doc, &["pricer"]);
        assert!(diags.is_empty(), "{diags:?}");
        let (keymap, diags) = build_keymap(&[doc], default_mod(), &registry());
        assert!(diags.is_empty(), "every key spells and every action is registered: {diags:?}");
        keymap
    }

    fn resolve(spec: &str, mode: &str) -> Option<String> {
        let keymap = keymap();
        let stack = [
            KeyContext::new("workspace"),
            KeyContext::new("tile"),
            KeyContext::new("pricer").pair("mode", mode).counts(),
        ];
        let mut m = Matcher::default();
        let mut out = None;
        for part in spec.split(' ') {
            let ks = parse_keystroke(part, default_mod()).unwrap();
            if let MatchResult::Matched { action, .. } = m.press(&keymap, ks, &stack) {
                out = Some(action.0);
            }
        }
        out
    }

    #[test]
    fn the_fragment_binds_every_action_but_the_palette_only_ones() {
        let fragment: toml::Table = toml::from_str(DEFAULT_KEYMAP).unwrap();
        let mut bound = std::collections::BTreeSet::new();
        for group in fragment["bindings"].as_array().unwrap() {
            for (_, action) in group["keys"].as_table().unwrap() {
                bound.insert(action.as_str().unwrap().to_string());
            }
        }
        let unbound: Vec<&str> = ACTIONS.iter().map(|(id, _)| *id).filter(|id| !bound.contains(*id)).collect();
        assert_eq!(unbound, NO_DEFAULT_KEY);
        let _ = keymap();
    }

    #[test]
    fn sequences_resolve_and_y_y_is_reachable() {
        assert_eq!(resolve("y y", "normal").as_deref(), Some("pricer::yank_row"));
        assert_eq!(resolve("y c", "normal").as_deref(), Some("pricer::yank_col"));
        assert_eq!(resolve("g p", "normal").as_deref(), Some("pricer::group"));
        assert_eq!(resolve("g g", "normal").as_deref(), Some("pricer::top"));
        assert_eq!(resolve("d d", "normal").as_deref(), Some("pricer::delete"));
        assert_eq!(resolve("z shift+r", "normal").as_deref(), Some("pricer::expand_all"));
        assert_eq!(resolve("enter", "entry").as_deref(), Some("pricer::commit"));
        assert_eq!(resolve("up", "entry").as_deref(), Some("pricer::insert_up"));
        assert_eq!(resolve("shift+up", "insert").as_deref(), Some("pricer::insert_up_big"));
        assert_eq!(resolve(".", "menu").as_deref(), Some("pricer::menu_close"));
    }
}
```

If `Keymap`, `Matcher`, `MatchResult` or `parse_keystroke` live at a different path, copy the imports from `geode-marketdata/src/content.rs`'s `mod tests` — that file is the working reference for these exact calls.

- [ ] **Step 4: `delegate.rs` — the table delegate**

```rust
//! The tile's `TableDelegate` (line-pricer spec §8.2): a prepared
//! `Rc<GridModel>` swapped wholesale by `PricerTile::install_model`, a
//! mirror of the tile's cursor, and (Tasks 9–10) mirrors of the open entry
//! field and cell editor. The tile's own state is the truth; nothing here
//! decides anything. Column 0 is the tree column (indent, chevron,
//! shorthand), pinned left; the cursor never enters it.

use crate::grid::{GridModel, GridRowKind};
use crate::paint::Paints;
use geode_shell::fonts;
use geode_shell::shell::control::{self, PointerStates as _};
use gpui::prelude::*;
use gpui::{App, ClickEvent, Context, EventEmitter, SharedString, TextAlign, Window, div, px};
use gpui_component::table::{Column, ColumnFixed, TableDelegate, TableState};
use gpui_component::{ActiveTheme as _, Theme};
use std::rc::Rc;

/// The tree column: pixels, like every width here (the vocabulary's own
/// known gap); not resizable, since a dragged width has nowhere to live.
const TREE_WIDTH: f32 = 260.0;
const INDENT: f32 = 14.0;
pub(crate) const TREE_COL: usize = 0;

/// A chevron click, re-implemented from the blotter (spec §8.2: "the
/// blotter's idiom re-implemented, nothing lifted"); the tile toggles
/// the package at this grid row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChevronClicked(pub usize);

impl EventEmitter<ChevronClicked> for TableState<SheetDelegate> {}

pub struct SheetDelegate {
    pub(crate) model: Rc<GridModel>,
    /// `(grid row, plan column)`; `None` with no cursor row.
    pub(crate) cursor: Option<(usize, usize)>,
    pub(crate) paints: Paints,
    chevron: Option<(control::ControlInputs, control::ControlPaint)>,
}

impl SheetDelegate {
    pub(crate) fn new(theme: &Theme) -> Self {
        SheetDelegate {
            model: Rc::new(GridModel::default()),
            cursor: None,
            paints: Paints::derive(theme),
            chevron: None,
        }
    }

    /// The plan column behind table column `col_ix`; `None` is the tree.
    pub(crate) fn plan_col(col_ix: usize) -> Option<usize> {
        col_ix.checked_sub(1)
    }

    fn chevron_states(&mut self, theme: &Theme) -> control::ControlPaint {
        let inputs = control::ControlInputs::new(theme, control::Rest::Bare, theme.table, theme.muted_foreground);
        match &self.chevron {
            Some((have, paint)) if *have == inputs => *paint,
            _ => {
                let paint = control::control_paint(&inputs);
                self.chevron = Some((inputs, paint));
                paint
            }
        }
    }
}

impl TableDelegate for SheetDelegate {
    fn columns_count(&self, _cx: &App) -> usize {
        1 + self.model.columns.len()
    }

    fn rows_count(&self, _cx: &App) -> usize {
        self.model.rows.len()
    }

    /// Read only on prepare and `TableState::refresh`, which is why every
    /// model swap goes through `install_model`.
    fn column(&self, col_ix: usize, _cx: &App) -> Column {
        let Some(c) = Self::plan_col(col_ix).and_then(|i| self.model.columns.get(i)) else {
            return Column {
                key: SharedString::from("__tree"),
                name: SharedString::from("line"),
                align: TextAlign::Left,
                sort: None,
                width: px(TREE_WIDTH),
                fixed: Some(ColumnFixed::Left),
                movable: false,
                resizable: false,
                ..Column::default()
            };
        };
        Column {
            key: c.label.clone(),
            name: c.label.clone(),
            align: if c.right { TextAlign::Right } else { TextAlign::Left },
            sort: None,
            width: px(c.width),
            movable: false,
            resizable: false,
            ..Column::default()
        }
    }

    fn render_th(&mut self, col_ix: usize, _window: &mut Window, cx: &mut Context<TableState<Self>>) -> impl IntoElement {
        let column = self.column(col_ix, cx);
        div()
            .size_full()
            .flex()
            .items_center()
            .when(matches!(column.align, TextAlign::Right), |el| el.justify_end())
            .font_family(fonts::MONO)
            .debug_selector(|| format!("pricer-th-{col_ix}"))
            .child(column.name)
    }

    /// One prepared cell. Nothing is formatted or allocated here beyond
    /// the `debug_selector` closure (dropped unevaluated outside tests):
    /// the text is a `SharedString` refcount out of the model, the colours
    /// `Copy` reads of the `Paints` memo, which the tile re-derives on a
    /// theme change rather than per cell.
    fn render_td(
        &mut self,
        row_ix: usize,
        col_ix: usize,
        _window: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        let paints = self.paints;
        // One `Rc` clone, so `chevron_states(&mut self)` can run while a
        // row is borrowed.
        let model = Rc::clone(&self.model);
        let Some(row) = model.rows.get(row_ix) else {
            return div().into_any_element();
        };
        let package = matches!(row.kind, GridRowKind::Package { .. });
        let (active_border, chevron_text, radius) = {
            let t = cx.theme();
            (t.table_active_border, t.muted_foreground, t.radius_tokens().sm)
        };
        let base = div()
            .size_full()
            .flex()
            .items_center()
            .font_family(fonts::MONO)
            .whitespace_nowrap()
            .overflow_hidden()
            .when(package, |el| el.bg(paints.package_ground))
            .debug_selector(|| format!("pricer-cell-{row_ix}-{col_ix}"));
        let Some(plan_col) = Self::plan_col(col_ix) else {
            // The tree column: indent by depth, a chevron on a package,
            // then the row's shorthand.
            let mut el = base
                .pl(px(row.depth as f32 * INDENT))
                .text_color(paints.text(crate::core::CellState::Own, package));
            if let GridRowKind::Package { open } = row.kind {
                let states = self.chevron_states(cx.theme());
                el = el.child(
                    div()
                        .id(("pricer-chevron", row_ix))
                        .w(px(14.))
                        .rounded(radius)
                        .text_color(chevron_text)
                        .pointer_states(states)
                        .debug_selector(|| format!("pricer-chevron-{row_ix}"))
                        .on_click(cx.listener(move |this, e: &ClickEvent, _window, cx| {
                            cx.stop_propagation();
                            // A double-click toggles once (the blotter's rule).
                            if e.click_count() > 1 {
                                return;
                            }
                            this.set_selected_row(row_ix, cx);
                            cx.emit(ChevronClicked(row_ix));
                        }))
                        .child(if open { "▾" } else { "▸" }),
                );
            }
            return el.child(row.tree.clone()).into_any_element();
        };
        let at_cursor = self.cursor == Some((row_ix, plan_col));
        let right = model.columns.get(plan_col).is_some_and(|c| c.right);
        base.when(right, |el| el.justify_end())
            .when(at_cursor, |el| el.border_1().border_color(active_border))
            .when_some(row.cells.get(plan_col), |el, cell| {
                el.text_color(paints.text(cell.state, package)).child(cell.text.clone())
            })
            .into_any_element()
    }
}
```

`render_td` returns `AnyElement` because the two arms build different element types; `AnyElement` satisfies the trait's `impl IntoElement`. If `radius_tokens().sm` is not `Copy` in the pinned component, read it inside the chevron arm instead. Tasks 9 and 10 add the entry field and the cell editor to this function.

- [ ] **Step 5: The header's render functions**

Append to `header.rs`:

```rust
use geode_shell::module::StackHandle;
use geode_shell::shell::chip::{Tone, chip_paint};
use geode_shell::shell::scale;
use geode_shell::tiling::TileId;
use gpui::prelude::*;
use gpui::{FontWeight, IntoElement, div};
use gpui_component::{Theme, h_flex};

pub(crate) fn render(h: &HeaderModel, theme: &Theme, stack: Option<&StackHandle>, tile: TileId) -> impl IntoElement {
    let muted = theme.muted_foreground;
    let warn = chip_paint(theme, Tone::WarningText).text;
    let chip = chip_paint(theme, Tone::Neutral);
    h_flex()
        .w_full()
        .h(scale::design(HEADER_HEIGHT))
        .items_center()
        .gap_3()
        .px_2()
        .text_sm()
        .text_color(muted)
        .border_b_1()
        .border_color(theme.border)
        .children(stack.and_then(|s| s.marker(theme, tile)))
        .child(div().font_weight(FontWeight::BOLD).text_color(theme.foreground).child(h.name.clone()))
        .child(h.view.clone())
        .children(h.shifts.iter().map(|s| {
            div()
                .px_1()
                .rounded(theme.radius_tokens().sm)
                .when_some(chip.fill, |el, fill| el.bg(fill))
                .text_color(chip.text)
                .child(s.clone())
        }))
        .child(div().flex_1())
        .when_some(h.notice.clone(), |el, n| {
            el.child(div().text_color(warn).debug_selector(|| "pricer-notice".into()).child(n))
        })
        .when_some(h.pricing.clone(), |el, p| el.child(p))
        .child(h.pricer.clone())
        .when_some(if h.stale { h.time_stale.clone() } else { h.time.clone() }, |el, t| {
            el.child(div().when(h.stale, |el| el.text_color(warn)).child(t))
        })
}

/// Always laid out, text or not, so the table's height never changes
/// (spec §8.3). Every footer line is a user error or a failure, so it
/// paints in danger text.
pub(crate) fn render_footer(text: Option<&SharedString>, theme: &Theme) -> impl IntoElement {
    h_flex()
        .w_full()
        .h(scale::design(FOOTER_HEIGHT))
        .px_2()
        .text_sm()
        .border_t_1()
        .border_color(theme.border)
        .text_color(chip_paint(theme, Tone::DangerText).text)
        .debug_selector(|| "pricer-footer".into())
        .children(text.cloned())
}
```

- [ ] **Step 6: The tile, test harness first**

`crates/geode-pricer/src/tile.rs` — write the `mod tests` harness and this task's tests first, then the tile. The harness copies `geode-timeseries/src/tile.rs:2217-2830`'s shape (a `Built` slot out of the window closure, a `Harness` driven through the trait, the receiver held in a `RefCell<Option<_>>` so a test can close it):

```rust
#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::content::{PricerFactory, PricerSettings};
    use crate::core::{Edit, Place, RowSpec, Sheet, Views, parse, to_rows};
    use crate::store::MemorySheetStore;
    use geode_core::groupings::GroupingSlots;
    use geode_core::log::LogLevels;
    use geode_core::pricing::{PriceOutcome, PriceParams, PriceResult};
    use geode_core::query::QueryKey;
    use geode_core::scopes::SavedScopes;
    use geode_data::{DataHandle, Request};
    use geode_shell::actions::ActionId;
    use geode_shell::diagnostics::Diagnostics;
    use geode_shell::frame::Frame;
    use geode_shell::module::{Delivery, ModuleFactory, TileContent};
    use geode_shell::tiling::TileId;
    use gpui::{Entity, VisualTestContext};
    use std::cell::RefCell;
    use std::rc::Rc;
    use std::sync::mpsc::Receiver;

    pub(crate) const TILE: u64 = 5;

    struct Built {
        content: Box<dyn TileContent>,
        tile: Entity<PricerTile>,
        frame: Entity<Frame>,
        diagnostics: Entity<Diagnostics>,
    }

    pub(crate) struct Harness {
        pub tile: Entity<PricerTile>,
        pub content: Box<dyn TileContent>,
        pub frame: Entity<Frame>,
        pub diagnostics: Entity<Diagnostics>,
        pub factory: Rc<PricerFactory>,
        pub store: MemorySheetStore,
        pub data: DataHandle,
        rx: RefCell<Option<Receiver<Request>>>,
    }

    /// A sheet named `book` in a fresh store, built from shorthand lines,
    /// and the session record that restores it.
    pub(crate) fn seeded(lines: &[&str]) -> (MemorySheetStore, toml::Table) {
        let mut s = Sheet::new("book");
        let rows: Vec<RowSpec> = lines.iter().map(|l| parse(l).unwrap()).collect();
        s.apply(Edit::Insert { place: Place::Root { at: 0 }, rows }).unwrap();
        let store = MemorySheetStore::default();
        assert!(store.save("book", to_rows(&s).unwrap()));
        let mut t = toml::Table::new();
        t.insert("sheet".into(), "book".into());
        (store, t)
    }

    pub(crate) fn open(cx: &mut gpui::TestAppContext) -> (Harness, VisualTestContext) {
        open_full(cx, None, MemorySheetStore::default(), PricerSettings::default())
    }

    pub(crate) fn open_seeded(cx: &mut gpui::TestAppContext, lines: &[&str]) -> (Harness, VisualTestContext) {
        let (store, record) = seeded(lines);
        let (h, mut vcx) = open_full(cx, Some(record), store, PricerSettings::default());
        h.visible(&mut vcx, true);
        (h, vcx)
    }

    pub(crate) fn open_full(
        cx: &mut gpui::TestAppContext,
        restored: Option<toml::Table>,
        store: MemorySheetStore,
        settings: PricerSettings,
    ) -> (Harness, VisualTestContext) {
        cx.update(gpui_component::init);
        cx.update(geode_shell::shell::dialog::init_reclaimed_keybindings);
        cx.update(crate::init);
        let (data, rx) = DataHandle::for_tests();
        let factory = Rc::new(PricerFactory::new(data.clone(), Rc::new(store.clone()), Views::builtin(), settings));
        let slot: Rc<RefCell<Option<Built>>> = Rc::new(RefCell::new(None));
        let window = cx
            .update(|cx| {
                let slot = slot.clone();
                let factory = factory.clone();
                cx.open_window(gpui::WindowOptions::default(), |window, cx| {
                    let frame = cx.new(|_| Frame::new(GroupingSlots::default(), SavedScopes::new(), None));
                    let diagnostics = cx.new(|_| Diagnostics::new(LogLevels::default()));
                    let occupant = factory.create(TileId(TILE), restored.as_ref(), frame.clone(), diagnostics.clone(), window, cx);
                    let tile = occupant.view.clone().downcast::<PricerTile>().unwrap();
                    *slot.borrow_mut() = Some(Built { content: occupant.content, tile: tile.clone(), frame, diagnostics });
                    // `Root` is load-bearing: gpui-component registers the
                    // focused `InputState` on it (Tasks 9–10's fields).
                    cx.new(|cx| gpui_component::Root::new(tile, window, cx))
                })
            })
            .unwrap();
        let mut vcx = VisualTestContext::from_window(window.into(), cx);
        let built = slot.borrow_mut().take().expect("the factory built one");
        vcx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        (
            Harness {
                tile: built.tile,
                content: built.content,
                frame: built.frame,
                diagnostics: built.diagnostics,
                factory,
                store,
                data,
                rx: RefCell::new(Some(rx)),
            },
            vcx,
        )
    }

    impl Harness {
        pub fn command(&self, vcx: &mut VisualTestContext, line: &str) -> Result<(), String> {
            vcx.update(|window, cx| self.content.command(line, window, cx))
        }
        pub fn dispatch(&self, vcx: &mut VisualTestContext, verb: &str, count: Option<u32>) -> bool {
            let id = ActionId(format!("pricer::{verb}"));
            vcx.update(|window, cx| self.content.dispatch(&id, count, window, cx))
        }
        pub fn visible(&self, vcx: &mut VisualTestContext, visible: bool) {
            vcx.update(|_, cx| self.content.set_visible(visible, cx));
        }
        pub fn close_channel(&self) {
            self.rx.borrow_mut().take();
        }
        /// Every request since the last drain, `Cancel` included.
        pub fn requests(&self) -> Vec<Request> {
            match self.rx.borrow().as_ref() {
                Some(rx) => rx.try_iter().collect(),
                None => Vec::new(),
            }
        }
        /// The price batches since the last drain, in order.
        pub fn prices(&self) -> Vec<PriceParams> {
            self.requests()
                .into_iter()
                .filter_map(|r| match r {
                    Request::Price(p) => Some(p),
                    _ => None,
                })
                .collect()
        }
        pub fn deliver(&self, vcx: &mut VisualTestContext, outcome: PriceOutcome) {
            vcx.update(|window, cx| self.content.deliver(Delivery::Price(outcome), window, cx));
        }
        /// Answer `params` in full: every line `Ok(result(price))`.
        pub fn answer(&self, vcx: &mut VisualTestContext, params: &PriceParams, price: f64) {
            self.deliver(
                vcx,
                PriceOutcome {
                    key: params.key,
                    tag: params.tag,
                    submitted: std::time::Instant::now(),
                    results: params.lines.iter().map(|l| (l.id, l.revision, Ok(result(price)))).collect(),
                },
            );
        }
        pub fn title(&self, vcx: &mut VisualTestContext) -> String {
            vcx.update(|_, cx| self.content.title(cx).to_string())
        }
        pub fn mode(&self, vcx: &mut VisualTestContext) -> String {
            vcx.update(|_, cx| self.content.key_context(cx).get("mode").unwrap_or("").to_string())
        }
        pub fn serialize(&self, vcx: &mut VisualTestContext) -> toml::Table {
            vcx.update(|_, cx| self.content.serialize(cx))
        }
        /// Column 0's text per grid row — the tree the table paints.
        pub fn tree(&self, vcx: &VisualTestContext) -> Vec<String> {
            self.tile.read_with(vcx, |t, _| t.model.rows.iter().map(|r| r.tree.to_string()).collect())
        }
        pub fn columns(&self, vcx: &VisualTestContext) -> Vec<String> {
            self.tile.read_with(vcx, |t, _| t.model.columns.iter().map(|c| c.label.to_string()).collect())
        }
        /// One cell's painted text, by grid row and column label.
        pub fn cell(&self, vcx: &VisualTestContext, row: usize, column: &str) -> String {
            self.tile.read_with(vcx, |t, _| {
                let c = t.model.columns.iter().position(|c| c.label.as_ref() == column).expect("column");
                t.model.rows[row].cells[c].text.to_string()
            })
        }
        pub fn header(&self, vcx: &VisualTestContext) -> Vec<String> {
            self.tile.read_with(vcx, |t, _| t.header.texts())
        }
        pub fn notice(&self, vcx: &VisualTestContext) -> Option<String> {
            self.tile.read_with(vcx, |t, _| t.header.notice.as_ref().map(|n| n.to_string()))
        }
        pub fn footer(&self, vcx: &VisualTestContext) -> Option<String> {
            self.tile.read_with(vcx, |t, _| t.footer_text.as_ref().map(|n| n.to_string()))
        }
        /// `(grid row, plan column)` of the cursor.
        pub fn cursor(&self, vcx: &VisualTestContext) -> Option<(usize, usize)> {
            self.tile.read_with(vcx, |t, _| t.cursor_row().map(|r| (r, t.cursor.col)))
        }
        pub fn sheet_len(&self, vcx: &VisualTestContext) -> usize {
            self.tile.read_with(vcx, |t, _| t.sheet.len())
        }
        pub fn draw(&self, vcx: &mut VisualTestContext) {
            vcx.update(|window, cx| {
                let _ = window.draw(cx);
            });
        }
    }

    pub(crate) fn result(price: f64) -> PriceResult {
        PriceResult { price, delta: 0.5, gamma: 0.01, vega: 1.0, theta: -0.5, rho: 0.1 }
    }

    // ---- Task 6 ----

    #[gpui::test]
    fn the_factory_is_kind_pricer_and_a_fresh_tile_opens_untitled_1(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        assert_eq!(h.factory.kind(), "pricer");
        assert_eq!(h.factory.contexts(), vec!["pricer"]);
        assert_eq!(h.title(&mut vcx), "pricer · untitled-1");
        assert_eq!(h.mode(&mut vcx), "normal");
        assert_eq!(h.columns(&vcx)[0], "qty", "the vanilla view");
        // A second tile skips the name the first holds.
        let second = vcx.update(|window, cx| {
            h.factory.create(TileId(TILE + 1), None, h.frame.clone(), h.diagnostics.clone(), window, cx)
        });
        assert_eq!(vcx.update(|_, cx| second.content.title(cx).to_string()), "pricer · untitled-2");
    }

    #[gpui::test]
    fn a_restored_sheet_loads_its_rows_cursor_and_expansion(cx: &mut gpui::TestAppContext) {
        let (store, mut record) = seeded(&["SPX Z26 5000 C", "-5 SPX Z26 4800/5200 CS", "SPX Z26 4000 P"]);
        // Ids are 1 (A), 2 (the package), 3–4 (legs), 5 (B).
        record.insert("cursor".into(), toml::Value::Integer(5));
        record.insert("expanded".into(), toml::Value::Array(vec![toml::Value::Integer(2)]));
        record.insert("view".into(), "barrier".into());
        let (h, mut vcx) = open_full(cx, Some(record), store, PricerSettings::default());
        assert_eq!(h.title(&mut vcx), "pricer · book");
        assert_eq!(h.tree(&vcx).len(), 5, "the package is open, so its legs show");
        assert_eq!(h.cursor(&vcx).map(|c| c.0), Some(4), "the cursor is on line 5");
        assert!(
            !h.columns(&vcx).contains(&"barrier".to_string()),
            "the DOCUMENT's view (vanilla) wins over the record's (barrier)"
        );
        assert!(h.notice(&vcx).is_none());
    }

    #[gpui::test]
    fn a_restored_name_with_no_document_opens_empty_under_that_name(cx: &mut gpui::TestAppContext) {
        let mut record = toml::Table::new();
        record.insert("sheet".into(), "gone".into());
        record.insert("view".into(), "barrier".into());
        let (h, mut vcx) = open_full(cx, Some(record), MemorySheetStore::default(), PricerSettings::default());
        assert_eq!(h.title(&mut vcx), "pricer · gone");
        assert_eq!(h.notice(&vcx).as_deref(), Some("sheet 'gone' was not found; opened empty"));
        assert!(h.columns(&vcx).contains(&"barrier".to_string()), "the record's view, with no document to say otherwise");
    }

    #[gpui::test]
    fn a_pending_load_paints_loading_until_the_rows_arrive(cx: &mut gpui::TestAppContext) {
        let (store, record) = seeded(&["SPX Z26 5000 C"]);
        let rows = store.get("book").unwrap();
        store.set_pending(true);
        let (h, mut vcx) = open_full(cx, Some(record), store, PricerSettings::default());
        assert_eq!(h.notice(&vcx).as_deref(), Some("loading…"));
        assert_eq!(h.sheet_len(&vcx), 0);
        // Part 4's `Delivery::Query` arm calls exactly this.
        h.tile.update(&mut vcx, |t, cx| t.loaded(Ok(Some(rows)), cx));
        assert_eq!(h.sheet_len(&vcx), 1);
        assert!(h.notice(&vcx).is_none());
    }

    #[gpui::test]
    fn the_session_record_round_trips_the_tile(cx: &mut gpui::TestAppContext) {
        let (store, mut record) = seeded(&["SPX Z26 5000 C", "SPX Z26 4000 P"]);
        record.insert("cursor".into(), toml::Value::Integer(2));
        let (h, mut vcx) = open_full(cx, Some(record), store.clone(), PricerSettings::default());
        h.command(&mut vcx, "view barrier").unwrap();
        let saved = h.serialize(&mut vcx);
        let r = crate::session::Record::from_table(&saved);
        assert_eq!(r.sheet.as_deref(), Some("book"));
        assert_eq!(r.view.as_deref(), Some("barrier"));
        assert_eq!(r.cursor, Some(crate::core::LineId(2)));
    }

    #[gpui::test]
    fn colon_view_switches_the_columns_and_an_unknown_view_is_refused(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_seeded(cx, &["SPX Z26 5000 C"]);
        assert!(!h.columns(&vcx).contains(&"barrier".to_string()));
        h.command(&mut vcx, "view barrier").unwrap();
        assert!(h.columns(&vcx).contains(&"barrier".to_string()));
        assert_eq!(
            h.command(&mut vcx, "view nope"),
            Err("no view 'nope' (have: vanilla, barrier)".into())
        );
        let words = h.tile.read_with(&vcx, |t, _| t.completions("view ", 5));
        assert_eq!(words, vec!["vanilla", "barrier"]);
    }

    #[gpui::test]
    fn a_reloaded_views_doc_reaches_an_open_tile(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_seeded(cx, &["SPX Z26 5000 C"]);
        let doc = geode_core::config::merge_docs(
            "pricer_views",
            &[geode_core::config::LayerDoc::builtin("pricer_views", "[slim]\ncolumns = [\"qty\", \"price\"]\n").unwrap()],
        );
        let (views, diags) = Views::from_doc(&doc);
        assert!(diags.is_empty());
        vcx.update(|_, cx| h.factory.reload(views, None, std::time::Duration::from_secs(60), cx));
        assert_eq!(h.columns(&vcx), vec!["qty", "price"], "the sheet's `vanilla` is gone, so the first view shows");
        assert_eq!(h.notice(&vcx).as_deref(), Some("view 'vanilla' is not defined; showing 'slim'"));
        assert_eq!(h.factory.settings().refresh, None);
    }

    /// Planning decision 6: a visible pricer submits no view query, so it
    /// must arrive at a flip barrier itself or every other following tile
    /// waits out the deadline (`geode-diagnostics`' own test, copied).
    #[gpui::test]
    fn the_tile_answers_a_flip_barrier_it_has_nothing_coming_for(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.frame.update(&mut vcx, |f, cx| {
            f.set_scope(geode_core::scope::Scope { text: Some("spx".into()), ..Default::default() });
            f.open_flip([QueryKey(TILE)], std::time::Instant::now());
            cx.notify();
        });
        assert!(!h.frame.read_with(&vcx, |f, _| f.barrier_open()));
    }

    #[gpui::test]
    fn a_closed_tile_gives_its_sheet_name_back(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        let (factory, frame, diagnostics) = (h.factory.clone(), h.frame.clone(), h.diagnostics.clone());
        drop(h);
        vcx.update(|window, _| window.remove_window());
        vcx.run_until_parked();
        drop(vcx);
        // A tile in a fresh window takes `untitled-1` again.
        let title = cx.update(|cx| {
            let mut title = String::new();
            cx.open_window(gpui::WindowOptions::default(), |window, cx| {
                let o = factory.create(TileId(TILE + 2), None, frame.clone(), diagnostics.clone(), window, cx);
                title = o.content.title(cx).to_string();
                cx.new(|cx| gpui_component::Root::new(o.view, window, cx))
            })
            .unwrap();
            title
        });
        assert_eq!(title, "pricer · untitled-1");
    }
}
```

`geode_core::scope::Scope`'s path and `Frame::set_scope` follow `geode-diagnostics/src/tile.rs:836-850`; copy that test's imports if they differ.

- [ ] **Step 7: Run to see them fail**

Run: `cargo test -p geode-pricer tile::`
Expected: FAIL to compile (`PricerTile`, `crate::init` do not exist).

- [ ] **Step 8: Implement the tile**

`crates/geode-pricer/src/tile.rs`, above the tests:

```rust
//! The line-pricer tile (spec §8): one `Sheet`, an `Rc<GridModel>`
//! installed into a `DataTable`, a header and a footer.
//!
//! **One door per kind of change.** A request-changing edit goes through
//! `apply_edit` (Task 8), which records its undo; a delivery through
//! `deliver`; a tick through `tick`. Each ends at `rebuild`, which builds
//! the grid model (never in `render`), installs it, re-prepares the
//! header and notifies.

use crate::content::{PricerSettings, Shared};
use crate::core::commands::{self, Command};
use crate::core::sheet::{LineId, Sheet};
use crate::core::storage::from_rows;
use crate::core::tree::Expansion;
use crate::core::views::ColumnPlan;
use crate::delegate::{ChevronClicked, SheetDelegate};
use crate::grid::{GridModel, GridRowKind};
use crate::header::{self, HeaderInputs, HeaderModel};
use crate::session::Record;
use crate::store::Loaded;
use geode_core::clock::Clock;
use geode_core::document::DocumentRows;
use geode_core::pricing::PriceOutcome;
use geode_core::query::QueryKey;
use geode_data::DataHandle;
use geode_shell::actions::ActionId;
use geode_shell::frame::Frame;
use geode_shell::keymap::KeyContext;
use geode_shell::module::{FindEvent, StackHandle};
use geode_shell::tiling::TileId;
use gpui::prelude::*;
use gpui::{App, Context, Entity, SharedString, Window, div};
use gpui_component::table::{DataTable, TableEvent, TableState};
use gpui_component::{ActiveTheme as _, Size, v_flex};
use std::rc::Rc;

pub(crate) const LOADING: &str = "loading…";

/// The cursor by line identity (planning decision 10): an edit elsewhere,
/// a delivery or an expansion never moves it. `last_row` is where it was,
/// for the fallback when its line goes away.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Cursor {
    pub line: Option<LineId>,
    /// A plan column (the tree column is never a cursor target).
    pub col: usize,
    pub last_row: usize,
}

pub struct PricerTile {
    pub(crate) id: TileId,
    frame: Entity<Frame>,
    pub(crate) data: DataHandle,
    pub(crate) shared: Rc<Shared>,
    pub(crate) sheet: Sheet,
    pub(crate) expansion: Expansion,
    pub(crate) plan: ColumnPlan,
    pub(crate) model: Rc<GridModel>,
    pub(crate) table: Entity<TableState<SheetDelegate>>,
    pub(crate) cursor: Cursor,
    pub(crate) visible: bool,
    pub(crate) loading: bool,
    /// Transient header notice (a load failure, a refused request).
    pub(crate) notice: Option<SharedString>,
    /// The view fallback's standing notice (`resolve_plan`).
    view_notice: Option<SharedString>,
    /// A user error for the footer (spec §8.3); cleared by the next verb.
    pub(crate) footer: Option<SharedString>,
    /// What the footer paints: `footer`, else the cursor row's failure.
    pub(crate) footer_text: Option<SharedString>,
    pub(crate) header: HeaderModel,
    title: SharedString,
    stack: Option<StackHandle>,
    pub(crate) clock: Clock,
}

fn app_clock(cx: &App) -> Clock {
    cx.try_global::<geode_shell::clock::AppClock>()
        .map(|c| c.0)
        .unwrap_or_else(|| Clock::machine().0)
}

/// The first `untitled-N` with no document and no open tile (spec §7.4).
fn untitled(shared: &Shared) -> String {
    (1..)
        .map(|n| format!("untitled-{n}"))
        .find(|name| !shared.open.borrow().contains(name) && !shared.store.contains(name))
        .expect("an unbounded range finds a free name")
}

/// An empty sheet under `name`, carrying the record's view and refresh —
/// what a restore shows when there is no document to say otherwise.
fn fallback(name: &str, record: &Record) -> Sheet {
    let mut s = Sheet::new(name);
    if let Some(v) = &record.view {
        s.view = v.clone();
    }
    if let Some(r) = record.refresh {
        s.refresh = r;
    }
    s
}

impl PricerTile {
    pub fn new(
        id: TileId,
        frame: Entity<Frame>,
        data: DataHandle,
        shared: Rc<Shared>,
        restored: Option<&toml::Table>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let record = restored.map(Record::from_table).unwrap_or_default();
        let mut notices: Vec<String> = Vec::new();
        let name = match record.sheet.as_deref() {
            Some(n) if !shared.open.borrow().contains(n) => n.to_string(),
            Some(n) => {
                let fresh = untitled(&shared);
                notices.push(format!("sheet '{n}' is open in another tile; opened {fresh}"));
                fresh
            }
            None => untitled(&shared),
        };
        shared.open.borrow_mut().insert(name.clone());
        let (sheet, loading) = match shared.store.load(&name) {
            Loaded::Rows(rows) => match from_rows(&name, &rows) {
                Ok(mut s) => {
                    s.mark_all_stale();
                    (s, false)
                }
                Err(e) => {
                    notices.push(format!("sheet '{name}' did not load: {e}"));
                    (fallback(&name, &record), false)
                }
            },
            Loaded::Missing => {
                if record.sheet.is_some() && notices.is_empty() {
                    notices.push(format!("sheet '{name}' was not found; opened empty"));
                }
                (fallback(&name, &record), false)
            }
            Loaded::Pending => {
                notices.push(LOADING.to_string());
                (fallback(&name, &record), true)
            }
        };
        let mut expansion = Expansion::from_ids(record.expanded.iter().copied());
        expansion.retain_packages(&sheet);

        let delegate = SheetDelegate::new(cx.theme());
        let table = cx.new(|cx| {
            TableState::new(delegate, window, cx)
                .row_selectable(true)
                .col_selectable(false)
                .cell_selectable(true)
                .row_header(false)
                .loop_selection(false)
                .col_resizable(false)
                .col_movable(false)
                .sortable(false)
        });
        cx.subscribe_in(&table, window, |this, _, event: &TableEvent, window, cx| {
            this.on_table_event(event, window, cx)
        })
        .detach();
        cx.subscribe(&table, |this, _, event: &ChevronClicked, cx| this.toggle_grid_row(event.0, cx))
            .detach();
        // Planning decision 6: arrive at every flip barrier at once.
        cx.observe(&frame, |this, frame, cx| {
            let key = QueryKey(this.id.0);
            let now = frame.read(cx).versions();
            if frame.read(cx).barrier_wants(key, now) {
                frame.update(cx, |f, cx| {
                    if f.arrived(key, now) {
                        cx.notify();
                    }
                });
            }
        })
        .detach();
        // The paints are a per-theme memo (planning decision 14): re-derived
        // here, once per theme change, never per cell.
        cx.observe_global::<gpui_component::Theme>(|this, cx| {
            let paints = crate::paint::Paints::derive(cx.theme());
            this.table.update(cx, |t, cx| {
                t.delegate_mut().paints = paints;
                cx.notify();
            });
        })
        .detach();
        // `priced_at` cells and the header time follow the app clock.
        cx.observe_global::<geode_shell::clock::AppClock>(|this, cx| {
            this.clock = app_clock(cx);
            this.rebuild(cx);
        })
        .detach();
        // A closed tile gives its name back (spec §7.4's open set). Tasks
        // 8 and 12 add the cancel and the final save here.
        cx.on_release(|this: &mut PricerTile, _cx| {
            this.shared.open.borrow_mut().remove(&this.sheet.name);
        })
        .detach();

        let cursor = Cursor {
            line: record.cursor,
            col: 0,
            last_row: 0,
        };
        let mut this = PricerTile {
            id,
            frame,
            data,
            shared,
            sheet,
            expansion,
            plan: ColumnPlan::default(),
            model: Rc::new(GridModel::default()),
            table,
            cursor,
            visible: false,
            loading,
            notice: (!notices.is_empty()).then(|| notices.join("; ").into()),
            view_notice: None,
            footer: None,
            footer_text: None,
            header: HeaderModel::default(),
            title: SharedString::default(),
            stack: None,
            clock: app_clock(cx),
        };
        this.resolve_plan();
        this.rebuild(cx);
        this
    }

    // ---- what the shell reads ----------------------------------------

    /// `normal` until Tasks 9–11 add `entry`, `insert` and `menu`.
    pub fn key_context(&self) -> KeyContext {
        KeyContext::new("pricer").pair("mode", self.mode()).counts()
    }

    pub(crate) fn mode(&self) -> &'static str {
        "normal"
    }

    /// Does one of THIS tile's own fields hold window focus? (Tasks 9–10
    /// add the entry field, the cell editor and the choice field.)
    pub fn holds_focus(&self, window: &Window, cx: &App) -> bool {
        let _ = (window, cx);
        false
    }

    pub fn title(&self) -> SharedString {
        self.title.clone()
    }

    pub fn serialize(&self) -> toml::Table {
        Record {
            sheet: Some(self.sheet.name.clone()),
            view: Some(self.sheet.view.clone()),
            refresh: Some(self.sheet.refresh),
            cursor: self.cursor.line,
            expanded: self.expansion.ids().collect(),
        }
        .to_table()
    }

    pub fn set_stack(&mut self, stack: Option<StackHandle>, cx: &mut Context<Self>) {
        self.stack = stack;
        cx.notify();
    }

    /// Task 8 adds the reprice on show and the cancel on hide.
    pub fn set_visible(&mut self, visible: bool, cx: &mut Context<Self>) {
        self.visible = visible;
        cx.notify();
    }

    /// Task 7 fills this in.
    pub fn find(&mut self, event: FindEvent, window: &mut Window, cx: &mut Context<Self>) {
        let _ = (event, window, cx);
    }

    /// Task 8 fills this in.
    pub fn deliver(&mut self, outcome: PriceOutcome, cx: &mut Context<Self>) {
        let _ = (outcome, cx);
    }

    /// A `Pending` load's answer (planning decision 7): Part 4's
    /// `Delivery::Query` arm calls this with the decoded document.
    pub fn loaded(&mut self, answer: Result<Option<DocumentRows>, String>, cx: &mut Context<Self>) {
        if !self.loading {
            return;
        }
        self.loading = false;
        self.notice = None;
        let name = self.sheet.name.clone();
        match answer {
            Ok(Some(rows)) => match from_rows(&name, &rows) {
                Ok(mut s) => {
                    s.mark_all_stale();
                    self.sheet = s;
                }
                Err(e) => self.notice = Some(format!("sheet '{name}' did not load: {e}").into()),
            },
            Ok(None) => self.notice = Some(format!("sheet '{name}' was not found; opened empty").into()),
            Err(e) => self.notice = Some(format!("sheet '{name}' did not load: {e}").into()),
        }
        self.expansion.retain_packages(&self.sheet);
        self.resolve_plan();
        self.rebuild(cx);
    }

    /// A reload reached this tile (planning decision 20).
    pub(crate) fn config_changed(&mut self, cx: &mut Context<Self>) {
        self.resolve_plan();
        self.rebuild(cx);
    }

    // ---- verbs ----------------------------------------------------------

    /// Every normal-mode verb (Tasks 7–11 add arms). A verb this tile
    /// handles clears the footer first, so a stale refusal never outlives
    /// the next keystroke.
    pub fn dispatch(
        &mut self,
        action: &ActionId,
        count: Option<u32>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(verb) = action.0.strip_prefix("pricer::") else {
            return false;
        };
        let _ = (count, window);
        match verb {
            "escape" => {
                self.footer = None;
                self.notice = None;
            }
            _ => return false,
        }
        self.sync_cursor(cx);
        self.rebuild_chrome();
        cx.notify();
        true
    }

    pub fn command(&mut self, line: &str, window: &mut Window, cx: &mut Context<Self>) -> Result<(), String> {
        let _ = window;
        match commands::parse(line)? {
            Command::View(name) => self.set_view(&name, cx),
            // Tasks 8 and 11 replace this arm verb by verb.
            _ => Err("not built yet".into()),
        }
    }

    pub fn completions(&self, line: &str, cursor: usize) -> Vec<String> {
        let views: Vec<String> = self.shared.views.borrow().names().map(str::to_string).collect();
        let mut unds: Vec<String> = (0..self.sheet.len())
            .filter_map(|r| self.sheet.instrument(r).map(|i| i.underlying().to_string()))
            .collect();
        unds.sort();
        unds.dedup();
        commands::completions(line, cursor, &views, &unds)
    }

    fn set_view(&mut self, name: &str, cx: &mut Context<Self>) -> Result<(), String> {
        {
            let views = self.shared.views.borrow();
            if views.get(name).is_none() {
                let have: Vec<&str> = views.names().collect();
                return Err(format!("no view '{name}' (have: {})", have.join(", ")));
            }
        }
        self.sheet.view = name.to_string();
        self.resolve_plan();
        self.rebuild(cx);
        Ok(())
    }

    // ---- the one rebuild path ------------------------------------------

    /// The sheet's view as a column plan, or the first loaded view with a
    /// standing notice (the sheet keeps its own `view`, so a view that
    /// comes back on the next reload is used again).
    pub(crate) fn resolve_plan(&mut self) {
        let views = self.shared.views.borrow();
        let (plan, notice) = match views.get(&self.sheet.view) {
            Some(v) => (ColumnPlan::build(v), None),
            None => match views.names().next().and_then(|n| views.get(n)) {
                Some(v) => (
                    ColumnPlan::build(v),
                    Some(format!("view '{}' is not defined; showing '{}'", self.sheet.view, v.name).into()),
                ),
                None => (ColumnPlan::default(), Some("no pricer views are defined".into())),
            },
        };
        drop(views);
        self.plan = plan;
        self.view_notice = notice;
        self.cursor.col = self.cursor.col.min(self.plan.columns.len().saturating_sub(1));
    }

    /// Model, table, chrome, notify — after every change that moves what
    /// the grid shows.
    pub(crate) fn rebuild(&mut self, cx: &mut Context<Self>) {
        self.model = Rc::new(GridModel::build(&self.sheet, &self.expansion, &self.plan, self.entry_place(), self.clock));
        self.install_model(cx);
        self.rebuild_chrome();
        cx.notify();
    }

    /// Task 9 answers the open entry field's place.
    pub(crate) fn entry_place(&self) -> Option<crate::core::Place> {
        None
    }

    /// The only way a model reaches the table (spec §8.2).
    pub(crate) fn install_model(&mut self, cx: &mut Context<Self>) {
        let model = Rc::clone(&self.model);
        self.table.update(cx, |t, cx| {
            t.delegate_mut().model = model;
            t.refresh(cx);
        });
        self.sync_cursor(cx);
    }

    pub(crate) fn rebuild_chrome(&mut self) {
        let settings: PricerSettings = self.shared.settings.borrow().clone();
        let notice = self.notice.clone().or_else(|| self.view_notice.clone());
        self.header = header::prepare(HeaderInputs { sheet: &self.sheet, notice, settings: &settings, clock: self.clock });
        self.title = format!("pricer · {}", self.sheet.name).into();
        self.footer_text = self.footer.clone().or_else(|| {
            let row = self.cursor_row().and_then(|r| self.model.rows[r].row)?;
            match self.sheet.state(row) {
                crate::core::LineState::Failed(m) => Some(m.clone().into()),
                _ => None,
            }
        });
    }

    // ---- the cursor -------------------------------------------------------

    /// Grid rows a cursor may sit on (never the entry placeholder).
    pub(crate) fn cursor_rows(&self) -> impl Iterator<Item = usize> + '_ {
        self.model
            .rows
            .iter()
            .enumerate()
            .filter(|(_, r)| r.kind != GridRowKind::Entry)
            .map(|(i, _)| i)
    }

    pub(crate) fn cursor_row(&self) -> Option<usize> {
        self.cursor.line.and_then(|id| self.model.grid_row_of(id))
    }

    /// Point the cursor at grid row `row` (clamped to a cursor row).
    pub(crate) fn set_cursor_row(&mut self, row: usize) {
        let rows: Vec<usize> = self.cursor_rows().collect();
        let Some(&target) = rows.iter().rev().find(|r| **r <= row).or(rows.first()) else {
            self.cursor.line = None;
            return;
        };
        self.cursor.line = self.model.rows[target].id;
        self.cursor.last_row = target;
    }

    /// A cursor whose line went away falls back to the row at its old
    /// index (planning decision 10).
    fn reconcile_cursor(&mut self) {
        match self.cursor_row() {
            Some(r) => self.cursor.last_row = r,
            None => self.set_cursor_row(self.cursor.last_row),
        }
    }

    /// Mirror the cursor into the table: column before row, so the
    /// component ends in row mode (the market-data order).
    pub(crate) fn sync_cursor(&mut self, cx: &mut Context<Self>) {
        self.reconcile_cursor();
        let row = self.cursor_row();
        let col = self.cursor.col;
        self.table.update(cx, |t, cx| {
            t.delegate_mut().cursor = row.map(|r| (r, col));
            match row {
                Some(r) => {
                    t.set_selected_col(col + 1, cx);
                    t.set_selected_row(r, cx);
                    t.scroll_to_row(r, cx);
                }
                None => t.clear_selection(cx),
            }
        });
    }

    fn on_table_event(&mut self, event: &TableEvent, window: &mut Window, cx: &mut Context<Self>) {
        let _ = window;
        if let TableEvent::SelectCell(row, col) = event {
            self.set_cursor_row(*row);
            if let Some(c) = SheetDelegate::plan_col(*col) {
                self.cursor.col = c;
            }
            self.sync_cursor(cx);
            self.rebuild_chrome();
            cx.notify();
        }
        // `SelectRow`/`SelectColumn` are what `sync_cursor` itself emits:
        // deliberately unmatched. Task 10 adds `DoubleClickedCell`.
    }

    /// Task 7's tree verbs; the chevron click lands here.
    pub(crate) fn toggle_grid_row(&mut self, row: usize, cx: &mut Context<Self>) {
        let _ = (row, cx);
    }
}

impl gpui::Render for PricerTile {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Staleness is a compare per frame, never a format (spec §9.4).
        let stale_after = self.shared.settings.borrow().stale_after;
        self.header.stale = self.header.last_priced.is_some_and(|at| {
            chrono::Utc::now().signed_duration_since(at).to_std().unwrap_or_default() > stale_after
        });
        let theme = cx.theme();
        let header = header::render(&self.header, theme, self.stack.as_ref(), self.id);
        let body = div()
            .flex_1()
            .min_h_0()
            .w_full()
            .child(DataTable::new(&self.table).with_size(Size::XSmall).bordered(false).stripe(false));
        let footer = header::render_footer(self.footer_text.as_ref(), theme);
        v_flex()
            .size_full()
            .debug_selector(|| format!("tile-content-{}", self.id.0))
            .child(header)
            .child(body)
            .child(footer)
    }
}
```

In `lib.rs` add the modules and `init` (the market-data copy; this crate may not depend on it):

```rust
pub mod content;
pub mod delegate;
pub mod header;
pub mod session;
pub mod tile;

/// Rebind `DataTable`'s own navigation keys to `NoAction` inside the
/// table's context, so the tile's fragment owns them (the blotter's and
/// the market-data panel's reason, copied: a module may not depend on a
/// sibling). Called once at startup, beside the other modules' inits.
pub fn init(cx: &mut gpui::App) {
    const CONTEXT: Option<&str> = Some("DataTable");
    cx.bind_keys(
        [
            "escape", "up", "down", "left", "right", "home", "end", "pageup", "pagedown", "tab", "shift-tab",
        ]
        .into_iter()
        .map(|key| gpui::KeyBinding::new(key, gpui::NoAction, CONTEXT)),
    );
}
```

If `cx.on_release` does not accept that closure shape in the pinned gpui (`Context::on_release(&self, impl FnOnce(&mut T, &mut App) + 'static) -> Subscription`), check `gpui-0.x/src/app/context.rs` in the registry and adapt; it is the one gpui call in this plan with no in-repo precedent. The test `a_closed_tile_gives_its_sheet_name_back` is what proves the route.

- [ ] **Step 9: Run the tests**

Run: `cargo test -p geode-pricer`
Expected: PASS — session (2), header (2), content (2), tile (8), plus Tasks 1–5.

- [ ] **Step 10: Gate and commit**

Run: `cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`

```bash
git add crates/geode-pricer/
git commit -m "feat(pricer): the hosted tile — factory, keymap, delegate, header, restore and naming

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---
### Task 7: Normal-mode navigation — motions, tree verbs, the chevron, yank and find

**Files:**
- Modify: `crates/geode-pricer/src/tile.rs` (`dispatch`, `find`, `toggle_grid_row`, new helpers, new fields `register`, `find`)
- Test: `crates/geode-pricer/src/tile.rs` (`mod tests`)

**Interfaces:**
- Consumes: Task 6's `Cursor`, `cursor_rows`, `cursor_row`, `set_cursor_row`, `sync_cursor`, `rebuild`, `rebuild_chrome`; Task 3's `Expansion`, `clip::spec_of`; `geode_shell::vimfind::{find_match, FindDirection}`; `gpui::ClipboardItem`.
- Produces: `PricerTile` fields `pub(crate) register: Option<RowSpec>` (Task 11's `p` reads it) and `find: Option<FindState>`; `fn cursor_sheet_row(&self) -> Option<usize>`; `fn expand_at_cursor(&mut self, open: Option<bool>, cx)`; `pub(crate) const HALF_PAGE: usize = 5; pub(crate) const FULL_PAGE: usize = 10;`. Test helpers `centre_of`, `click_at` (for Tasks 10–11).

- [ ] **Step 1: Write the failing tests**

Append to `mod tests` in `tile.rs` (the two pointer helpers are `geode-timeseries/src/tile.rs:2829-2862`, copied):

```rust
    pub(crate) fn centre_of(vcx: &mut VisualTestContext, selector: &str) -> gpui::Point<gpui::Pixels> {
        vcx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        // `debug_bounds` wants a `&'static str`: leaked, a test-only cost.
        let selector: &'static str = Box::leak(selector.to_string().into_boxed_str());
        vcx.debug_bounds(selector).unwrap_or_else(|| panic!("{selector} is painted")).center()
    }

    /// A left mouse-down/up pair carrying `click_count` (gpui's own
    /// `simulate_click` hardwires 1).
    pub(crate) fn click_at(vcx: &mut VisualTestContext, at: gpui::Point<gpui::Pixels>, click_count: usize) {
        vcx.simulate_event(gpui::MouseDownEvent {
            position: at,
            modifiers: gpui::Modifiers::default(),
            button: gpui::MouseButton::Left,
            click_count,
            first_mouse: false,
        });
        vcx.simulate_event(gpui::MouseUpEvent {
            position: at,
            modifiers: gpui::Modifiers::default(),
            button: gpui::MouseButton::Left,
            click_count,
        });
    }

    /// [A, P(L1, L2), B] with P closed: grid rows A=0, P=1, B=2.
    const BOOK: [&str; 3] = ["SPX Z26 5000 C", "-5 SPX Z26 4800/5200 CS", "SPX Z26 4000 P"];

    // ---- Task 7 ----

    #[gpui::test]
    fn motions_move_the_cursor_and_never_into_the_tree_column(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_seeded(cx, &BOOK);
        assert_eq!(h.cursor(&vcx), Some((0, 0)), "a restore with no cursor lands on the first row");
        h.dispatch(&mut vcx, "down", Some(2));
        assert_eq!(h.cursor(&vcx), Some((2, 0)));
        h.dispatch(&mut vcx, "down", None);
        assert_eq!(h.cursor(&vcx), Some((2, 0)), "clamped at the last row");
        h.dispatch(&mut vcx, "top", None);
        assert_eq!(h.cursor(&vcx), Some((0, 0)));
        h.dispatch(&mut vcx, "bottom", None);
        assert_eq!(h.cursor(&vcx), Some((2, 0)));
        h.dispatch(&mut vcx, "left", None);
        assert_eq!(h.cursor(&vcx), Some((2, 0)), "column 0 is the first plan column; the tree is not a target");
        h.dispatch(&mut vcx, "right", Some(3));
        assert_eq!(h.cursor(&vcx), Some((2, 3)));
        h.dispatch(&mut vcx, "last_col", None);
        let last = h.columns(&vcx).len() - 1;
        assert_eq!(h.cursor(&vcx), Some((2, last)));
        h.dispatch(&mut vcx, "first_col", None);
        assert_eq!(h.cursor(&vcx), Some((2, 0)));
        h.dispatch(&mut vcx, "page_up", None);
        assert_eq!(h.cursor(&vcx), Some((0, 0)), "half a page (5) clamps at the top");
    }

    #[gpui::test]
    fn tree_verbs_open_and_close_packages_and_a_leg_collapses_to_its_package(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_seeded(cx, &BOOK);
        h.dispatch(&mut vcx, "down", None);
        h.dispatch(&mut vcx, "toggle", None);
        assert_eq!(h.tree(&vcx).len(), 5, "space opens the package under the cursor");
        h.dispatch(&mut vcx, "down", None);
        assert_eq!(h.cursor(&vcx).map(|c| c.0), Some(2), "on the first leg");
        h.dispatch(&mut vcx, "collapse", None);
        assert_eq!(h.tree(&vcx).len(), 3);
        assert_eq!(h.cursor(&vcx).map(|c| c.0), Some(1), "z c on a leg closes its package and lands on it");
        h.dispatch(&mut vcx, "expand", None);
        assert_eq!(h.tree(&vcx).len(), 5);
        h.dispatch(&mut vcx, "down", None);
        h.dispatch(&mut vcx, "collapse_all", None);
        assert_eq!(h.tree(&vcx).len(), 3);
        assert_eq!(h.cursor(&vcx).map(|c| c.0), Some(1), "z M off a leg lands on its package");
        h.dispatch(&mut vcx, "expand_all", None);
        assert_eq!(h.tree(&vcx).len(), 5);
        let r = crate::session::Record::from_table(&h.serialize(&mut vcx));
        assert_eq!(r.expanded, vec![crate::core::LineId(2)], "the session carries the open package");
    }

    #[gpui::test]
    fn a_chevron_click_toggles_once_even_on_a_double_click(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_seeded(cx, &BOOK);
        let at = centre_of(&mut vcx, "pricer-chevron-1");
        click_at(&mut vcx, at, 1);
        h.draw(&mut vcx);
        assert_eq!(h.tree(&vcx).len(), 5);
        let at = centre_of(&mut vcx, "pricer-chevron-1");
        click_at(&mut vcx, at, 1);
        click_at(&mut vcx, at, 2);
        h.draw(&mut vcx);
        assert_eq!(h.tree(&vcx).len(), 3, "the second press of a double-click is ignored");
    }

    #[gpui::test]
    fn a_cell_click_moves_the_cursor_to_that_cell(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_seeded(cx, &BOOK);
        let at = centre_of(&mut vcx, "pricer-cell-2-3");
        click_at(&mut vcx, at, 1);
        h.draw(&mut vcx);
        assert_eq!(h.cursor(&vcx), Some((2, 2)), "table column 3 is plan column 2");
    }

    #[gpui::test]
    fn yy_yanks_the_rows_shorthand_and_yc_the_column(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_seeded(cx, &BOOK);
        h.dispatch(&mut vcx, "down", None);
        h.dispatch(&mut vcx, "yank_row", None);
        let clip = vcx.update(|_, cx| cx.read_from_clipboard().and_then(|c| c.text()));
        assert_eq!(clip.as_deref(), Some("-5 SPX Z26 4800/5200 CS"));
        assert!(h.tile.read_with(&vcx, |t, _| t.register.is_some()), "p puts what y y yanked");
        h.dispatch(&mut vcx, "right", Some(3)); // strike
        h.dispatch(&mut vcx, "yank_col", None);
        let clip = vcx.update(|_, cx| cx.read_from_clipboard().and_then(|c| c.text()));
        assert_eq!(clip.as_deref(), Some("5000\n\n4000"), "the package's strike is blank");
    }

    #[gpui::test]
    fn find_jumps_from_its_origin_repeats_with_n_and_escape_returns(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_seeded(cx, &BOOK);
        let find = |vcx: &mut VisualTestContext, e: FindEvent| {
            vcx.update(|window, cx| h.content.find(e, window, cx));
        };
        find(&mut vcx, FindEvent::Changed("4000".into()));
        assert_eq!(h.cursor(&vcx).map(|c| c.0), Some(2));
        find(&mut vcx, FindEvent::Committed("spx".into()));
        h.dispatch(&mut vcx, "find_next", None);
        assert_eq!(h.cursor(&vcx).map(|c| c.0), Some(0), "wraps");
        h.dispatch(&mut vcx, "find_prev", None);
        assert_eq!(h.cursor(&vcx).map(|c| c.0), Some(2));
        find(&mut vcx, FindEvent::Changed("cs".into()));
        find(&mut vcx, FindEvent::Cancelled);
        assert_eq!(h.cursor(&vcx).map(|c| c.0), Some(0), "escape returns to where `/` opened");
    }
```

The yank-column expectation assumes the vanilla plan's column 3 is `strike` (`qty, underlying, expiry, strike, …`) and `render_strike(5000)` spells `5000`; both are pinned by the core's own tests.

- [ ] **Step 2: Run to see them fail**

Run: `cargo test -p geode-pricer tile::tests`
Expected: the new tests FAIL (verbs answer `false`, the cursor does not move).

- [ ] **Step 3: Implement**

Add fields to `PricerTile` (initialised `None` in `new`):

```rust
    /// What `p`/`shift+p` put (Task 11): the last `y y` or `d d`.
    pub(crate) register: Option<crate::core::RowSpec>,
    find: Option<FindState>,
```

and near the top of `tile.rs`:

```rust
/// `ctrl+d`/`ctrl+u` and `ctrl+f`/`ctrl+b` steps — `vimnav`'s fixed ±5
/// and ±10, the market-data panel's own constants, times the count.
pub(crate) const HALF_PAGE: usize = 5;
pub(crate) const FULL_PAGE: usize = 10;

/// `/` over the tree column's text (spec §8.5): the vim jump model — a
/// sheet's rows are the trader's own order, so find moves the cursor and
/// never narrows.
struct FindState {
    /// Where `/` opened; `escape` returns here.
    origin: Cursor,
    /// The last committed query, for `n`/`N`.
    committed: Option<String>,
}
```

Replace `dispatch`'s body:

```rust
    pub fn dispatch(
        &mut self,
        action: &ActionId,
        count: Option<u32>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(verb) = action.0.strip_prefix("pricer::") else {
            return false;
        };
        let _ = window;
        let n = count.unwrap_or(1).max(1) as usize;
        self.footer = None;
        match verb {
            "down" => self.step_rows(n as isize),
            "up" => self.step_rows(-(n as isize)),
            "page_down" => self.step_rows((HALF_PAGE * n) as isize),
            "page_up" => self.step_rows(-((HALF_PAGE * n) as isize)),
            "page_down_full" => self.step_rows((FULL_PAGE * n) as isize),
            "page_up_full" => self.step_rows(-((FULL_PAGE * n) as isize)),
            "top" => self.jump_row(count.map(|c| c as usize).unwrap_or(1)),
            "bottom" => self.jump_row(count.map(|c| c as usize).unwrap_or(usize::MAX)),
            "left" => self.cursor.col = self.cursor.col.saturating_sub(n),
            "right" => {
                let last = self.plan.columns.len().saturating_sub(1);
                self.cursor.col = (self.cursor.col + n).min(last);
            }
            "first_col" => self.cursor.col = 0,
            "last_col" => self.cursor.col = self.plan.columns.len().saturating_sub(1),
            "toggle" => return self.tree_verb(None, cx),
            "expand" => return self.tree_verb(Some(true), cx),
            "collapse" => return self.tree_verb(Some(false), cx),
            "expand_all" | "collapse_all" => {
                if verb == "expand_all" {
                    self.expansion.open_all(&self.sheet);
                } else {
                    // Off a leg, the cursor lands on its package (it is
                    // about to disappear).
                    if let Some(p) = self.cursor_sheet_row().and_then(|r| self.sheet.parent(r)) {
                        self.cursor.line = Some(self.sheet.id(p));
                    }
                    self.expansion.close_all();
                }
                self.rebuild(cx);
                return true;
            }
            "yank_row" => {
                if let Some(row) = self.cursor_sheet_row() {
                    cx.write_to_clipboard(gpui::ClipboardItem::new_string(self.sheet.shorthand(row)));
                    self.register = Some(crate::core::clip::spec_of(&self.sheet, row));
                }
            }
            "yank_col" => {
                let col = self.cursor.col;
                let text = self
                    .model
                    .rows
                    .iter()
                    .filter(|r| r.kind != GridRowKind::Entry)
                    .map(|r| r.cells.get(col).map(|c| c.text.to_string()).unwrap_or_default())
                    .collect::<Vec<_>>()
                    .join("\n");
                cx.write_to_clipboard(gpui::ClipboardItem::new_string(text));
            }
            "find_next" => self.repeat_find(FindDirection::Forward, n),
            "find_prev" => self.repeat_find(FindDirection::Backward, n),
            "escape" => {
                self.find = None;
                self.notice = None;
            }
            _ => return false,
        }
        self.sync_cursor(cx);
        self.rebuild_chrome();
        cx.notify();
        true
    }

    /// The sheet row under the cursor.
    pub(crate) fn cursor_sheet_row(&self) -> Option<usize> {
        self.cursor_row().and_then(|r| self.model.rows[r].row)
    }

    fn step_rows(&mut self, delta: isize) {
        let rows: Vec<usize> = self.cursor_rows().collect();
        if rows.is_empty() {
            return;
        }
        let pos = self
            .cursor_row()
            .and_then(|r| rows.iter().position(|x| *x == r))
            .unwrap_or(0) as isize;
        let to = (pos + delta).clamp(0, rows.len() as isize - 1) as usize;
        self.set_cursor_row(rows[to]);
    }

    /// `g g` / `shift+g`: the first/last row, or the `count`th (1-based).
    fn jump_row(&mut self, nth: usize) {
        let rows: Vec<usize> = self.cursor_rows().collect();
        if let Some(r) = rows.get(nth.saturating_sub(1).min(rows.len().saturating_sub(1))) {
            self.set_cursor_row(*r);
        }
    }

    /// `space`/`z a` (`None`), `z o`, `z c` on the cursor's package — on a
    /// leg, its package; closing from a leg lands the cursor on the
    /// package, the blotter's `z c` rule. A line with no package does
    /// nothing.
    fn tree_verb(&mut self, open: Option<bool>, cx: &mut Context<Self>) -> bool {
        let Some(row) = self.cursor_sheet_row() else {
            return true;
        };
        let package = if self.sheet.is_package(row) {
            row
        } else if let Some(p) = self.sheet.parent(row) {
            p
        } else {
            return true;
        };
        let id = self.sheet.id(package);
        let now_open = match open {
            Some(o) => {
                self.expansion.set(id, o);
                o
            }
            None => self.expansion.toggle(id),
        };
        if !now_open {
            self.cursor.line = Some(id);
        }
        self.rebuild(cx);
        true
    }

    /// The chevron at grid row `row` (spec §8.2: its click is `space`).
    pub(crate) fn toggle_grid_row(&mut self, row: usize, cx: &mut Context<Self>) {
        self.set_cursor_row(row);
        self.tree_verb(None, cx);
    }

    fn row_labels(&self) -> Vec<String> {
        self.model.rows.iter().map(|r| r.tree.to_string()).collect()
    }

    fn repeat_find(&mut self, dir: FindDirection, count: usize) {
        let Some(query) = self.find.as_ref().and_then(|f| f.committed.clone()) else {
            return;
        };
        let labels = self.row_labels();
        if labels.is_empty() {
            return;
        }
        let mut at = self.cursor_row().unwrap_or(0);
        for _ in 0..count {
            let start = match dir {
                FindDirection::Forward => (at + 1) % labels.len(),
                FindDirection::Backward => (at + labels.len() - 1) % labels.len(),
            };
            match find_match(&labels, start, dir, &query) {
                Some(row) => at = row,
                None => return,
            }
        }
        self.set_cursor_row(at);
    }
```

Replace `find`:

```rust
    /// `/` (spec §8.5): every keystroke searches from the ORIGIN, so a
    /// lengthening query walks forward and a shortened one walks back
    /// (vim's incsearch); `escape` returns to the origin.
    pub fn find(&mut self, event: FindEvent, window: &mut Window, cx: &mut Context<Self>) {
        let _ = window;
        match event {
            FindEvent::Changed(query) => {
                let origin = match &self.find {
                    Some(f) => f.origin,
                    None => {
                        self.find = Some(FindState { origin: self.cursor, committed: None });
                        self.cursor
                    }
                };
                let from = origin.line.and_then(|id| self.model.grid_row_of(id)).unwrap_or(0);
                if let Some(row) = find_match(&self.row_labels(), from, FindDirection::Forward, &query) {
                    self.set_cursor_row(row);
                }
            }
            FindEvent::Committed(query) => {
                if let Some(f) = self.find.as_mut()
                    && !query.is_empty()
                {
                    f.committed = Some(query);
                } else if !query.is_empty() {
                    self.find = Some(FindState { origin: self.cursor, committed: Some(query) });
                }
            }
            FindEvent::Cancelled => {
                if let Some(f) = self.find.take() {
                    self.cursor = f.origin;
                }
            }
        }
        self.sync_cursor(cx);
        self.rebuild_chrome();
        cx.notify();
    }
```

Imports: `use geode_shell::vimfind::{FindDirection, find_match};`. The `set_cursor_row` from Task 6 already records `last_row`.

- [ ] **Step 4: Run the tests**

Run: `cargo test -p geode-pricer tile::tests`
Expected: PASS (Task 6's eight and these six).

- [ ] **Step 5: Gate and commit**

Run: `cargo fmt --check && cargo clippy -p geode-pricer --all-targets -- -D warnings && cargo test -p geode-pricer`

```bash
git add crates/geode-pricer/src/tile.rs
git commit -m "feat(pricer): cursor motions, tree verbs, chevron, yank and find

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---
### Task 8: Repricing — the edit door, submission, delivery, hide/show, retry, the refresh timer, `:price` and `:refresh`

**Files:**
- Modify: `crates/geode-pricer/src/tile.rs`
- Test: `crates/geode-pricer/src/tile.rs` (`mod tests`)

**Interfaces:**
- Consumes: `Sheet::{apply, stale_lines, request, overrides, deliver_all, mark_all_stale}`, `Delivered`, `UndoStack`, `DataHandle::{price, cancel}`, `geode_core::pricing::{PriceParams, PriceLine, PriceOutcome}`, `Refresh`.
- Produces (Tasks 9–12 call these):

```rust
pub(crate) const RETRY_AFTER: Duration = Duration::from_secs(1);
pub(crate) const REFUSED: &str = "pricing request refused: the data service is busy or gone; retrying";
impl PricerTile {
    pub(crate) fn apply_edit(&mut self, edit: Edit, cx: &mut Context<Self>) -> Result<(), EditError>;
    pub(crate) fn apply_edits(&mut self, edits: Vec<Edit>, cx: &mut Context<Self>) -> Result<(), EditError>;
    pub(crate) fn after_edit(&mut self, cx: &mut Context<Self>);   // retain, rebuild, submit, timer (Task 12 adds the save)
    pub(crate) fn submit(&mut self, cx: &mut Context<Self>);
    pub(crate) fn restart_timer(&mut self, cx: &mut Context<Self>);
    fn tick(&mut self, cx: &mut Context<Self>);
}
// fields: tag: u64, in_flight: HashMap<LineId, u64>, pub(crate) undo: UndoStack,
//         refresh_task: Option<Task<()>>, retry_task: Option<Task<()>>
```

- [ ] **Step 1: Write the failing tests**

Append to `mod tests`:

```rust
    // ---- Task 8 ----

    fn edit(h: &Harness, vcx: &mut VisualTestContext, e: Edit) {
        h.tile.update(vcx, |t, cx| t.apply_edit(e, cx)).unwrap();
    }

    fn new_strike(h: &Harness, vcx: &VisualTestContext, row: usize, strike: f64) -> Edit {
        let i = h.tile.read_with(vcx, |t, _| t.sheet.instrument(row).unwrap().clone());
        let geode_core::pricing::Instrument::Vanilla(mut v) = i else { panic!("vanilla") };
        v.strike = geode_core::pricing::Strike::Absolute(strike);
        Edit::SetInstrument { row, instrument: geode_core::pricing::Instrument::Vanilla(v) }
    }

    #[gpui::test]
    fn a_shown_tile_prices_every_stale_line_in_one_batch_and_the_answer_paints(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_seeded(cx, &BOOK);
        let batches = h.prices();
        assert_eq!(batches.len(), 1, "one PriceParams for the frame");
        let b = &batches[0];
        assert_eq!(b.key, QueryKey(TILE));
        assert_eq!(b.lines.iter().map(|l| l.id).collect::<Vec<_>>(), vec![1, 3, 4, 5], "lines only, never the package");
        assert_eq!(h.header(&vcx).iter().filter(|t| t.ends_with("pricing…")).count(), 1);
        h.answer(&mut vcx, b, 12.5);
        assert_eq!(h.cell(&vcx, 0, "price"), "12.50");
        assert_eq!(h.cell(&vcx, 1, "price"), "0.00", "−5 × 12.5 + 5 × 12.5: the package sums signed legs");
        assert!(!h.header(&vcx).iter().any(|t| t.ends_with("pricing…")));
        assert!(h.prices().is_empty(), "nothing left stale, nothing resubmitted");
    }

    #[gpui::test]
    fn a_request_changing_edit_resubmits_and_a_qty_edit_does_not(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_seeded(cx, &BOOK);
        let b = h.prices().remove(0);
        h.answer(&mut vcx, &b, 12.5);
        edit(&h, &mut vcx, Edit::SetQty { row: 0, qty: 7 });
        assert!(h.prices().is_empty(), "qty changes no request (spec §9.3)");
        let e = new_strike(&h, &vcx, 0, 5100.0);
        edit(&h, &mut vcx, e);
        let again = h.prices();
        assert_eq!(again.len(), 1);
        assert_eq!(again[0].lines.iter().map(|l| l.id).collect::<Vec<_>>(), vec![1], "only the edited line is stale");
        assert!(again[0].tag > b.tag);
    }

    /// Planning decision 4: a newer batch carries every stale line, so
    /// dropping the older batch's outcome whole loses nothing.
    #[gpui::test]
    fn an_older_submissions_outcome_is_dropped_whole(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_seeded(cx, &BOOK);
        let first = h.prices().remove(0);
        let e = new_strike(&h, &vcx, 0, 5100.0);
        edit(&h, &mut vcx, e);
        let second = h.prices().remove(0);
        assert_eq!(second.lines.len(), 4, "the new batch carries the old one's lines too");
        h.answer(&mut vcx, &first, 99.0);
        assert_eq!(h.cell(&vcx, 2, "price"), "", "the older tag installs nothing");
        h.answer(&mut vcx, &second, 12.5);
        assert_eq!(h.cell(&vcx, 2, "price"), "12.50");
    }

    #[gpui::test]
    fn an_answer_for_an_old_revision_leaves_the_line_stale_and_resubmits_it(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_seeded(cx, &BOOK);
        let first = h.prices().remove(0);
        let e = new_strike(&h, &vcx, 0, 5100.0);
        edit(&h, &mut vcx, e);
        let second = h.prices().remove(0);
        // The CURRENT tag, but line 1 answered at the revision before the
        // edit (an edit landed during the round trip, spec §9.2).
        h.deliver(
            &mut vcx,
            PriceOutcome {
                key: second.key,
                tag: second.tag,
                submitted: std::time::Instant::now(),
                results: first.lines.iter().map(|l| (l.id, l.revision, Ok(result(12.5)))).collect(),
            },
        );
        assert_eq!(h.cell(&vcx, 0, "price"), "", "line 1's answer is for an older request");
        assert_eq!(h.cell(&vcx, 2, "price"), "12.50", "the rest install");
        let again = h.prices();
        assert_eq!(again.len(), 1);
        assert_eq!(again[0].lines.iter().map(|l| l.id).collect::<Vec<_>>(), vec![1], "only line 1 goes again");
    }

    #[gpui::test]
    fn a_delivery_for_another_key_is_ignored(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_seeded(cx, &BOOK);
        let b = h.prices().remove(0);
        let mut other = b.clone();
        other.key = QueryKey(99);
        h.answer(&mut vcx, &other, 12.5);
        assert_eq!(h.cell(&vcx, 0, "price"), "");
    }

    #[gpui::test]
    fn a_failed_line_paints_a_dash_names_itself_in_the_footer_and_fails_its_package(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_seeded(cx, &BOOK);
        let b = h.prices().remove(0);
        h.deliver(
            &mut vcx,
            PriceOutcome {
                key: b.key,
                tag: b.tag,
                submitted: std::time::Instant::now(),
                results: b
                    .lines
                    .iter()
                    .map(|l| (l.id, l.revision, if l.id == 3 { Err("refused by the mock".into()) } else { Ok(result(1.0)) }))
                    .collect(),
            },
        );
        assert_eq!(h.cell(&vcx, 1, "price"), "—", "a failed leg fails its package");
        h.dispatch(&mut vcx, "down", None);
        assert!(h.footer(&vcx).unwrap().ends_with("refused by the mock"), "the cursor row's failure in the footer");
        h.dispatch(&mut vcx, "down", None);
        assert_eq!(h.footer(&vcx), None);
    }

    #[gpui::test]
    fn hide_cancels_by_key_and_prices_nothing_until_shown(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_seeded(cx, &BOOK);
        let _ = h.prices();
        h.visible(&mut vcx, false);
        assert!(h.requests().iter().any(|r| matches!(r, Request::Cancel { key } if *key == QueryKey(TILE))));
        let e = new_strike(&h, &vcx, 0, 5100.0);
        edit(&h, &mut vcx, e);
        assert!(h.prices().is_empty(), "a hidden tile keeps its stale marks and submits nothing");
        h.visible(&mut vcx, true);
        assert_eq!(h.prices().len(), 1, "and resubmits on show");
    }

    #[gpui::test]
    fn a_refused_submission_notices_and_retries_after_a_second(cx: &mut gpui::TestAppContext) {
        let (store, record) = seeded(&BOOK);
        let (h, mut vcx) = open_full(cx, Some(record), store, PricerSettings::default());
        h.close_channel();
        h.visible(&mut vcx, true);
        assert_eq!(h.notice(&vcx).as_deref(), Some(REFUSED));
        let tag = h.tile.read_with(&vcx, |t, _| t.tag);
        vcx.executor().advance_clock(RETRY_AFTER);
        vcx.run_until_parked();
        assert!(h.tile.read_with(&vcx, |t, _| t.tag) > tag, "the retry fired and asked again");
    }

    #[gpui::test]
    fn the_refresh_tick_marks_every_line_stale_and_submits(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_seeded(cx, &BOOK);
        let b = h.prices().remove(0);
        h.answer(&mut vcx, &b, 12.5);
        vcx.executor().advance_clock(std::time::Duration::from_secs(30));
        vcx.run_until_parked();
        let tick = h.prices();
        assert_eq!(tick.len(), 1);
        assert_eq!(tick[0].lines.len(), 4, "every line, at unchanged revisions");
        assert_eq!(
            tick[0].lines.iter().map(|l| l.revision).collect::<Vec<_>>(),
            b.lines.iter().map(|l| l.revision).collect::<Vec<_>>()
        );
    }

    #[gpui::test]
    fn colon_refresh_sets_this_sheets_interval_and_off_stops_it(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_seeded(cx, &BOOK);
        let b = h.prices().remove(0);
        h.answer(&mut vcx, &b, 12.5);
        h.command(&mut vcx, "refresh off").unwrap();
        vcx.executor().advance_clock(std::time::Duration::from_secs(120));
        vcx.run_until_parked();
        assert!(h.prices().is_empty());
        h.command(&mut vcx, "refresh 5s").unwrap();
        vcx.executor().advance_clock(std::time::Duration::from_secs(5));
        vcx.run_until_parked();
        assert_eq!(h.prices().len(), 1);
        let r = crate::session::Record::from_table(&h.serialize(&mut vcx));
        assert_eq!(r.refresh, Some(crate::core::Refresh::Every(std::time::Duration::from_secs(5))));
    }

    #[gpui::test]
    fn colon_price_reprices_everything_now(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_seeded(cx, &BOOK);
        let b = h.prices().remove(0);
        h.answer(&mut vcx, &b, 12.5);
        h.command(&mut vcx, "price").unwrap();
        assert_eq!(h.prices()[0].lines.len(), 4);
        assert!(h.dispatch(&mut vcx, "price", None), "the palette's action is the same verb");
    }

    #[gpui::test]
    fn a_missing_pricer_names_itself_in_the_header(cx: &mut gpui::TestAppContext) {
        let settings = PricerSettings { pricer: "vendor".into(), pricer_missing: true, ..PricerSettings::default() };
        let (h, vcx) = open_full(cx, None, MemorySheetStore::default(), settings);
        assert_eq!(h.notice(&vcx).as_deref(), Some("pricer \"vendor\" is not built into this binary"));
    }

    #[gpui::test]
    fn the_header_time_reads_the_app_clock(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_seeded(cx, &BOOK);
        let b = h.prices().remove(0);
        h.answer(&mut vcx, &b, 12.5);
        vcx.update(|_, cx| cx.set_global(geode_shell::clock::AppClock(geode_core::clock::Clock::utc())));
        let utc = h.tile.read_with(&vcx, |t, _| t.header.time.clone());
        vcx.update(|_, cx| {
            cx.set_global(geode_shell::clock::AppClock(geode_core::clock::Clock::in_zone_named("Asia/Tokyo")))
        });
        let tokyo = h.tile.read_with(&vcx, |t, _| t.header.time.clone());
        assert_ne!(utc, tokyo, "a zone change re-prepares the header");
    }
```

- [ ] **Step 2: Run to see them fail**

Run: `cargo test -p geode-pricer tile::tests`
Expected: the new tests FAIL (no submission, `apply_edit` missing).

- [ ] **Step 3: Implement**

Add fields to `PricerTile` (initialised `0`, `HashMap::new()`, `UndoStack::default()`, `None`, `None`):

```rust
    /// The latest submission's tag: an outcome with any other is dropped
    /// whole (spec §9.2).
    pub(crate) tag: u64,
    /// `id → revision` of the latest batch (planning decision 4): decides
    /// WHETHER to submit, never what — a batch always carries every stale line.
    in_flight: HashMap<LineId, u64>,
    pub(crate) undo: UndoStack,
    refresh_task: Option<Task<()>>,
    retry_task: Option<Task<()>>,
```

The door, submission, delivery and timer:

```rust
pub(crate) const RETRY_AFTER: Duration = Duration::from_secs(1);
pub(crate) const REFUSED: &str = "pricing request refused: the data service is busy or gone; retrying";

    /// The one edit door (global constraints): apply, record the undo,
    /// then everything an edit implies.
    pub(crate) fn apply_edit(&mut self, edit: Edit, cx: &mut Context<Self>) -> Result<(), EditError> {
        let undo = self.sheet.apply(edit)?;
        self.undo.record(undo);
        self.after_edit(cx);
        Ok(())
    }

    /// Several edits as ONE undo entry (`:spot clear`). On a refusal the
    /// ones already applied are taken back and nothing is recorded.
    pub(crate) fn apply_edits(&mut self, edits: Vec<Edit>, cx: &mut Context<Self>) -> Result<(), EditError> {
        let mut undos: Vec<Undo> = Vec::new();
        for e in edits {
            match self.sheet.apply(e) {
                Ok(u) => undos.push(u),
                Err(err) => {
                    for u in undos.iter().rev() {
                        let _ = self.sheet.undo(u);
                    }
                    self.after_edit(cx);
                    return Err(err);
                }
            }
        }
        if undos.is_empty() {
            return Ok(());
        }
        // Take back the LAST edit first.
        self.undo.record(Undo {
            inverse: undos.into_iter().rev().flat_map(|u| u.inverse).collect(),
        });
        self.after_edit(cx);
        Ok(())
    }

    /// What every edit, undo and redo implies: forget dead package ids,
    /// rebuild, reprice what changed, and make sure the timer runs once
    /// the sheet has a line. Task 12 adds the write-behind save.
    pub(crate) fn after_edit(&mut self, cx: &mut Context<Self>) {
        self.expansion.retain_packages(&self.sheet);
        self.rebuild(cx);
        self.submit(cx);
        if self.refresh_task.is_none() {
            self.restart_timer(cx);
        }
    }

    /// One `PriceParams` of every stale line, when some stale line is not
    /// already in flight at its current revision (spec §9.1, planning
    /// decision 4). A hidden or loading tile submits nothing.
    pub(crate) fn submit(&mut self, cx: &mut Context<Self>) {
        if !self.visible || self.loading {
            return;
        }
        let stale: Vec<usize> = self.sheet.stale_lines().collect();
        let needed = stale
            .iter()
            .any(|r| self.in_flight.get(&self.sheet.id(*r)) != Some(&self.sheet.revision(*r)));
        if !needed {
            return;
        }
        let lines: Vec<PriceLine> = stale
            .iter()
            .filter_map(|r| {
                self.sheet.request(*r).map(|request| PriceLine {
                    id: self.sheet.id(*r).0,
                    revision: self.sheet.revision(*r),
                    request,
                })
            })
            .collect();
        let flight: HashMap<LineId, u64> = lines.iter().map(|l| (LineId(l.id), l.revision)).collect();
        self.tag += 1;
        let queued = self.data.price(PriceParams {
            key: QueryKey(self.id.0),
            tag: self.tag,
            submitted: Instant::now(),
            overrides: self.sheet.overrides().clone(),
            lines,
        });
        if queued {
            self.in_flight = flight;
            self.retry_task = None;
            if self.notice.as_ref().is_some_and(|n| n.as_ref() == REFUSED) {
                self.notice = None;
            }
        } else {
            // Planning decision 5: nothing else would ever resubmit.
            self.in_flight.clear();
            self.notice = Some(REFUSED.into());
            self.arm_retry(cx);
        }
        self.rebuild_chrome();
        cx.notify();
    }

    fn arm_retry(&mut self, cx: &mut Context<Self>) {
        if self.retry_task.is_some() {
            return;
        }
        self.retry_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(RETRY_AFTER).await;
            let _ = this.update(cx, |t, cx| {
                t.retry_task = None;
                t.submit(cx);
            });
        }));
    }

    /// `Delivery::Price` for this tile (spec §9.2).
    pub fn deliver(&mut self, outcome: PriceOutcome, cx: &mut Context<Self>) {
        if outcome.key != QueryKey(self.id.0) || outcome.tag != self.tag {
            return;
        }
        let ids: Vec<(u64, u64)> = outcome.results.iter().map(|(id, rev, _)| (*id, *rev)).collect();
        let answers = self.sheet.deliver_all(
            outcome.results.into_iter().map(|(id, rev, r)| (LineId(id), rev, r)),
            Utc::now(),
        );
        for ((id, rev), answer) in ids.into_iter().zip(answers) {
            match answer {
                Delivered::Installed | Delivered::OldRevision { .. } => {}
                // Deleted mid-round-trip: ordinary (planning decision 19).
                Delivered::UnknownLine => tracing::debug!(
                    target: "geode::pricing",
                    tile = self.id.0, id, rev,
                    "price result for a line no longer on the sheet"
                ),
                // Bugs (spec §10.1): dropped and logged with the ids.
                Delivered::NotALine | Delivered::FutureRevision { .. } => tracing::warn!(
                    target: "geode::pricing",
                    tile = self.id.0, id, rev, answer = ?answer,
                    "price result dropped"
                ),
            }
        }
        // The latest batch is answered (a cancelled one partly): whatever
        // is still stale — an edit landed mid-flight, or a line the cancel
        // cut off — is resubmitted.
        self.in_flight.clear();
        self.rebuild(cx);
        self.submit(cx);
    }

    fn interval(&self) -> Option<Duration> {
        match self.sheet.refresh {
            Refresh::Every(d) => Some(d),
            Refresh::Off => None,
            Refresh::Default => self.shared.settings.borrow().refresh,
        }
    }

    /// The periodic reprice (spec §9.4): running only while visible;
    /// every tick marks every line stale and submits. Dropping the task
    /// stops it.
    pub(crate) fn restart_timer(&mut self, cx: &mut Context<Self>) {
        self.refresh_task = None;
        if !self.visible {
            return;
        }
        let Some(every) = self.interval() else {
            return;
        };
        self.refresh_task = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(every).await;
                if this.update(cx, |t, cx| t.tick(cx)).is_err() {
                    break;
                }
            }
        }));
    }

    /// A tick on an empty or loading sheet does nothing ("only while the
    /// sheet has a line"); the timer stays armed and costs one wake.
    fn tick(&mut self, cx: &mut Context<Self>) {
        if self.sheet.is_empty() || self.loading {
            return;
        }
        self.sheet.mark_all_stale();
        self.rebuild(cx);
        self.submit(cx);
    }

    fn reprice_all(&mut self, cx: &mut Context<Self>) {
        self.sheet.mark_all_stale();
        self.rebuild(cx);
        self.submit(cx);
    }
```

Replace `set_visible`:

```rust
    /// A show reprices what is stale and starts the timer; a hide cancels
    /// in flight by key and stops it, keeping the stale marks (spec §9.5).
    pub fn set_visible(&mut self, visible: bool, cx: &mut Context<Self>) {
        if self.visible == visible {
            return;
        }
        self.visible = visible;
        if visible {
            self.submit(cx);
            self.restart_timer(cx);
        } else {
            self.data.cancel(QueryKey(self.id.0));
            self.in_flight.clear();
            self.refresh_task = None;
            self.retry_task = None;
        }
        self.rebuild_chrome();
        cx.notify();
    }
```

In `dispatch`, add the palette verb:

```rust
            "price" => {
                self.reprice_all(cx);
                return true;
            }
```

In `command`, replace the catch-all with arms for `Price` and `Refresh`, keeping the catch-all for the verbs Task 11 wires:

```rust
            Command::Price => {
                self.reprice_all(cx);
                Ok(())
            }
            Command::Refresh(r) => {
                self.sheet.refresh = r;
                self.restart_timer(cx);
                self.rebuild_chrome();
                cx.notify();
                Ok(())
            }
            // Task 11 replaces this arm.
            Command::Shift { .. } | Command::Spot { .. } | Command::Group(_) | Command::Ungroup => {
                Err("not built yet".into())
            }
```

In `loaded`, after `self.rebuild(cx);` add `self.submit(cx);`. In `config_changed`, after `self.rebuild(cx);` add `self.restart_timer(cx);`. In `new`'s `on_release`, add `this.data.cancel(QueryKey(this.id.0));` before the name removal (spec §9.5: closing cancels).

Imports: `crate::core::edit::{Edit, EditError, Undo}`, `crate::core::sheet::{Delivered, Refresh}`, `crate::core::undo::UndoStack`, `geode_core::pricing::{PriceLine, PriceParams}`, `chrono::Utc`, `gpui::Task`, `std::collections::HashMap`, `std::time::{Duration, Instant}`.

- [ ] **Step 4: Run the tests**

Run: `cargo test -p geode-pricer tile::tests`
Expected: PASS. If the tick test sees two batches (the first answered batch plus the tick's), the `answer` did not clear `in_flight` — fix `deliver`, not the test.

- [ ] **Step 5: Gate and commit**

Run: `cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`

```bash
git add crates/geode-pricer/src/tile.rs
git commit -m "feat(pricer): reprice through Delivery::Price — the edit door, in-flight rule, timer, retry

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---
### Task 9: Entry mode — `o`/`shift+o`, the placeholder field, parse on `enter`, history

**Files:**
- Modify: `crates/geode-pricer/src/tile.rs`, `crates/geode-pricer/src/delegate.rs`
- Test: `crates/geode-pricer/src/tile.rs` (`mod tests`)

**Interfaces:**
- Consumes: Task 3's `entry::{place_for, next_place, history}`, Task 8's `apply_edit`, `core::shorthand::parse`, `gpui_component::input::{Input, InputState}`.
- Produces: `pub(crate) struct Entry { pub place: Place, pub input: Entity<InputState>, history: Vec<String>, history_ix: Option<usize> }`; `PricerTile` field `pub(crate) entry: Option<Entry>`; `fn open_entry(&mut self, below: bool, window, cx)`, `fn commit_entry(&mut self, window, cx)`, `pub(crate) fn close_entry(&mut self, window, cx)`, `fn step_history(&mut self, delta: isize, window, cx)`; `mode()` answers `entry`; `SheetDelegate` field `pub(crate) entry: Option<Entity<InputState>>`.

- [ ] **Step 1: Write the failing tests**

```rust
    // ---- Task 9 ----

    fn typed(h: &Harness, vcx: &mut VisualTestContext, text: &str) {
        vcx.simulate_input(text);
        h.draw(vcx);
    }

    fn focused(vcx: &mut VisualTestContext) -> bool {
        vcx.update(|window, cx| window.focused(cx).is_some())
    }

    /// Spec §12: `o`, a line, `enter` adds a row and submits one request.
    #[gpui::test]
    fn o_then_a_line_then_enter_adds_a_row_submits_it_and_opens_the_next_placeholder(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.visible(&mut vcx, true);
        h.dispatch(&mut vcx, "add_below", None);
        assert_eq!(h.mode(&mut vcx), "entry");
        assert!(focused(&mut vcx), "the field owns focus");
        typed(&h, &mut vcx, "-5 SPX Z26 5000 C");
        h.dispatch(&mut vcx, "commit", None);
        assert_eq!(h.sheet_len(&vcx), 1);
        let batches = h.prices();
        assert_eq!(batches.len(), 1);
        assert_eq!(batches[0].lines.len(), 1);
        assert_eq!(h.mode(&mut vcx), "entry", "a fresh placeholder opens below");
        assert_eq!(h.tile.read_with(&vcx, |t, _| t.model.entry_row()), Some(1));
        typed(&h, &mut vcx, "SPX Z26 4800/5200 CS");
        h.dispatch(&mut vcx, "commit", None);
        assert_eq!(h.sheet_len(&vcx), 4, "a package with its two legs");
        assert_eq!(h.tree(&vcx).len(), 5, "the typed package opens so its legs show, plus the placeholder");
    }

    #[gpui::test]
    fn a_parse_error_keeps_the_text_and_names_the_column(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.dispatch(&mut vcx, "add_below", None);
        typed(&h, &mut vcx, "SPX Z26 5000 CX");
        h.dispatch(&mut vcx, "commit", None);
        assert_eq!(h.sheet_len(&vcx), 0);
        assert_eq!(h.mode(&mut vcx), "entry");
        let footer = h.footer(&vcx).unwrap();
        assert!(footer.ends_with("(column 14)"), "{footer}");
        let text = h.tile.read_with(&vcx, |t, cx| t.entry.as_ref().unwrap().input.read(cx).value().to_string());
        assert_eq!(text, "SPX Z26 5000 CX", "the text is kept for fixing");
    }

    #[gpui::test]
    fn shift_o_on_a_leg_inserts_a_leg_before_it(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_seeded(cx, &BOOK);
        h.dispatch(&mut vcx, "down", None);
        h.dispatch(&mut vcx, "expand", None);
        h.dispatch(&mut vcx, "down", Some(2)); // the second leg
        h.dispatch(&mut vcx, "add_above", None);
        typed(&h, &mut vcx, "SPX Z26 5000 C");
        h.dispatch(&mut vcx, "commit", None);
        let legs = h.tile.read_with(&vcx, |t, _| t.sheet.children(1).len());
        assert_eq!(legs, 3);
        let middle = h.tile.read_with(&vcx, |t, _| t.sheet.shorthand(3));
        assert_eq!(middle, "SPX Z26 5000 C", "between the two legs");
    }

    #[gpui::test]
    fn a_package_typed_at_a_leg_place_is_refused_in_the_footer(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_seeded(cx, &BOOK);
        h.dispatch(&mut vcx, "down", None);
        h.dispatch(&mut vcx, "add_below", None); // a package row: its first leg
        typed(&h, &mut vcx, "SPX Z26 4800/5200 CS");
        h.dispatch(&mut vcx, "commit", None);
        assert_eq!(h.footer(&vcx).as_deref(), Some("a package cannot hold a package"));
        assert_eq!(h.mode(&mut vcx), "entry");
    }

    #[gpui::test]
    fn up_and_down_walk_the_sheets_own_lines_newest_first(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_seeded(cx, &BOOK);
        h.dispatch(&mut vcx, "add_below", None);
        let text = |vcx: &VisualTestContext| {
            h.tile.read_with(vcx, |t, cx| t.entry.as_ref().unwrap().input.read(cx).value().to_string())
        };
        h.dispatch(&mut vcx, "insert_up", None);
        assert_eq!(text(&vcx), "SPX Z26 4000 P");
        h.dispatch(&mut vcx, "insert_up", None);
        assert_eq!(text(&vcx), "-5 SPX Z26 4800/5200 CS");
        h.dispatch(&mut vcx, "insert_down", None);
        assert_eq!(text(&vcx), "SPX Z26 4000 P");
        h.dispatch(&mut vcx, "insert_down", None);
        assert_eq!(text(&vcx), "", "past the newest is an empty field");
    }

    #[gpui::test]
    fn escape_removes_the_placeholder_and_the_field_blurs_before_it_drops(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_seeded(cx, &BOOK);
        h.dispatch(&mut vcx, "add_below", None);
        assert!(focused(&mut vcx));
        h.dispatch(&mut vcx, "cancel", None);
        assert!(!focused(&mut vcx), "blurred, then dropped (CLAUDE.md)");
        assert_eq!(h.mode(&mut vcx), "normal");
        assert_eq!(h.tile.read_with(&vcx, |t, _| t.model.entry_row()), None);
        assert_eq!(h.sheet_len(&vcx), 5, "the sheet never held the placeholder");
    }

    #[gpui::test]
    fn a_click_on_the_table_cancels_the_entry_and_another_verb_closes_it(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_seeded(cx, &BOOK);
        h.dispatch(&mut vcx, "add_below", None);
        let at = centre_of(&mut vcx, "pricer-cell-0-2");
        click_at(&mut vcx, at, 1);
        h.draw(&mut vcx);
        assert_eq!(h.mode(&mut vcx), "normal", "a click cancels, never commits");
        h.dispatch(&mut vcx, "add_below", None);
        h.dispatch(&mut vcx, "down", None); // from the palette: not an entry verb
        assert_eq!(h.mode(&mut vcx), "normal");
        assert!(!focused(&mut vcx));
    }
```

The column-14 expectation: `"SPX Z26 5000 CX"` puts `CX` at byte offset 13 (the parser's offset is 0-based, the footer's column 1-based).

- [ ] **Step 2: Run to see them fail**

Run: `cargo test -p geode-pricer tile::tests`
Expected: the new tests FAIL (`add_below` is unhandled).

- [ ] **Step 3: Implement**

In `tile.rs`:

```rust
/// The entry field (spec §8.4): where its rows will land, the field, and
/// the sheet's own lines to walk with `up`/`down`.
pub(crate) struct Entry {
    pub place: Place,
    pub input: Entity<InputState>,
    history: Vec<String>,
    /// `None`: the trader's own text; `Some(i)`: showing `history[i]`.
    history_ix: Option<usize>,
}

const ENTRY_HINT: &str = "-5 SPX DEC26 95%/105% CS";
```

Field `pub(crate) entry: Option<Entry>` (initialised `None`). Make `entry_place` answer `self.entry.as_ref().map(|e| e.place)`. `mode()`:

```rust
    pub(crate) fn mode(&self) -> &'static str {
        if self.entry.is_some() { "entry" } else { "normal" }
    }
```

`holds_focus`:

```rust
    pub fn holds_focus(&self, window: &Window, cx: &App) -> bool {
        self.entry
            .as_ref()
            .is_some_and(|e| e.input.read(cx).focus_handle(cx).is_focused(window))
    }
```

The verbs:

```rust
    /// `o` / `shift+o`: a placeholder after (before) the cursor row, the
    /// field focused (spec §8.4). A leg place opens its package so the
    /// placeholder shows where it lands.
    fn open_entry(&mut self, below: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.loading {
            self.footer = Some("the sheet is still loading".into());
            return;
        }
        self.close_entry(window, cx);
        let place = place_for(&self.sheet, self.cursor_sheet_row(), below);
        if let Place::Leg { package, .. } = place {
            self.expansion.set(self.sheet.id(package), true);
        }
        let input = cx.new(|cx| InputState::new(window, cx).placeholder(ENTRY_HINT));
        input.read(cx).focus_handle(cx).focus(window, cx);
        self.entry = Some(Entry { place, input: input.clone(), history: history(&self.sheet), history_ix: None });
        self.table.update(cx, |t, _| t.delegate_mut().entry = Some(input));
        self.rebuild(cx);
    }

    /// `enter`: parse, insert, reprice, and open the next placeholder
    /// below what landed; a parse error or a refusal keeps the text and
    /// says why in the footer.
    fn commit_entry(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(entry) = self.entry.as_mut() else {
            return;
        };
        let text = entry.input.read(cx).value().to_string();
        let spec = match parse(&text) {
            Ok(spec) => spec,
            Err(e) => {
                self.footer = Some(format!("{} (column {})", e.message, e.offset + 1).into());
                self.rebuild_chrome();
                cx.notify();
                return;
            }
        };
        let at = entry.place;
        // Move the placeholder first so the edit's own rebuild paints it
        // below what landed; put it back on a refusal.
        entry.place = next_place(at, &spec);
        match self.apply_edit(Edit::Insert { place: at, rows: vec![spec.clone()] }, cx) {
            Ok(()) => {
                let first = match at {
                    Place::Root { at } => at,
                    Place::Leg { package, leg } => package + 1 + leg,
                };
                let id = self.sheet.id(first);
                if matches!(spec, RowSpec::Package { .. }) {
                    self.expansion.set(id, true);
                }
                self.cursor.line = Some(id);
                if let Some(entry) = self.entry.as_mut() {
                    entry.history = history(&self.sheet);
                    entry.history_ix = None;
                    entry.input.update(cx, |s, cx| s.set_value("", window, cx));
                }
                self.rebuild(cx);
            }
            Err(e) => {
                if let Some(entry) = self.entry.as_mut() {
                    entry.place = at;
                }
                self.footer = Some(e.to_string().into());
                self.rebuild(cx);
            }
        }
    }

    /// Blur only if OUR field holds focus, then drop it (the market-data
    /// rule, CLAUDE.md): an unblurred dead handle leaves the window
    /// focused on nothing and the shell's focus return never fires.
    pub(crate) fn close_entry(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(entry) = self.entry.take() else {
            return;
        };
        if entry.input.read(cx).focus_handle(cx).is_focused(window) {
            window.blur(cx);
        }
        self.table.update(cx, |t, _| t.delegate_mut().entry = None);
        self.rebuild(cx);
    }

    /// `up` walks back through the sheet's lines, `down` forward; past the
    /// newest is an empty field.
    fn step_history(&mut self, delta: isize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(entry) = self.entry.as_mut() else {
            return;
        };
        if entry.history.is_empty() {
            return;
        }
        let last = entry.history.len() as isize - 1;
        let next = match entry.history_ix {
            None if delta > 0 => Some(0),
            None => None,
            Some(i) => {
                let j = i as isize + delta;
                if j < 0 { None } else { Some(j.min(last) as usize) }
            }
        };
        entry.history_ix = next;
        let text = next.map(|i| entry.history[i].clone()).unwrap_or_default();
        entry.input.update(cx, |s, cx| s.set_value(text, window, cx));
    }
```

In `dispatch`, before the verb `match` (and after `self.footer = None`):

```rust
        // Any verb but the field's own four closes an open entry first
        // (a palette dispatch can arrive while it is open).
        if self.entry.is_some() && !matches!(verb, "commit" | "cancel" | "insert_up" | "insert_down") {
            self.close_entry(window, cx);
        }
```

and the arms (Task 10 extends `commit`, `cancel`, `insert_*` for the cell editor):

```rust
            "add_below" | "add_above" => {
                self.open_entry(verb == "add_below", window, cx);
                return true;
            }
            "commit" => {
                self.commit_entry(window, cx);
                return true;
            }
            "cancel" => {
                self.close_entry(window, cx);
                return true;
            }
            "insert_up" => {
                self.step_history(1, window, cx);
                return true;
            }
            "insert_down" => {
                self.step_history(-1, window, cx);
                return true;
            }
```

Keep `let _ = window;` out of `dispatch` now that it is used. In `on_table_event`, before handling `SelectCell`, close an open entry (a click anywhere cancels): `if self.entry.is_some() { self.close_entry(window, cx); }`.

In `delegate.rs` add `pub(crate) entry: Option<Entity<InputState>>` (initialised `None`) and, in `render_td`'s tree arm, before the chevron: when `row.kind == GridRowKind::Entry`, paint the field over the table's active-row ground and return:

```rust
            if row.kind == GridRowKind::Entry {
                let ground = cx.theme().table_active;
                return match &self.entry {
                    Some(input) => base
                        .bg(ground)
                        .debug_selector(|| "pricer-entry".into())
                        .child(div().flex_1().child(Input::new(input)))
                        .into_any_element(),
                    None => base.bg(ground).into_any_element(),
                };
            }
```

(imports: `gpui::Entity`, `gpui_component::input::{Input, InputState}`). A placeholder row's other cells are blank already (`GridModel::build`). If `table_active` is not the token name in the pinned theme, use the one the blotter's selected row paints with (`geode-blotter/src/delegate.rs`) — the spec asks for "the blotter's row ground".

Imports in `tile.rs`: `crate::core::entry::{history, next_place, place_for}`, `crate::core::shorthand::parse`, `crate::core::{Place, RowSpec}`, `gpui_component::input::InputState`, `gpui::Focusable as _` if `focus_handle` needs it.

- [ ] **Step 4: Run the tests**

Run: `cargo test -p geode-pricer tile::tests`
Expected: PASS.

- [ ] **Step 5: Gate and commit**

Run: `cargo fmt --check && cargo clippy -p geode-pricer --all-targets -- -D warnings && cargo test -p geode-pricer`

```bash
git add crates/geode-pricer/src/
git commit -m "feat(pricer): entry mode — o/O placeholder, shorthand on enter, history

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---
### Task 10: Insert mode — the cell editor, the choice typeahead, nudge, double-click

**Files:**
- Create: `crates/geode-pricer/src/popup.rs`
- Modify: `crates/geode-pricer/src/tile.rs`, `crates/geode-pricer/src/delegate.rs`, `crates/geode-pricer/src/lib.rs` (`pub mod popup;`)
- Test: `crates/geode-pricer/src/tile.rs` (`mod tests`)

**Interfaces:**
- Consumes: Task 2's `cell::{editor_for, commit, nudge, CellEditor, READ_ONLY}`, Task 8's `apply_edit`, Task 9's `close_entry`, `geode_shell::choice::{ChoiceList, DEFAULT_CAP}`, `geode_shell::vimnav::NavCommand`, `gpui_component::input::{Input, InputEvent, InputState}`.
- Produces:

```rust
// tile.rs
pub(crate) enum Editor {
    Text { line: LineId, col: usize, kind: ColumnKind, input: Entity<InputState> },
    Choice { line: LineId, col: usize, kind: ColumnKind, input: Entity<InputState>, list: ChoiceList, free: bool },
}
// PricerTile field: pub(crate) editor: Option<Editor>
fn begin_edit(&mut self, window, cx);
fn commit_edit(&mut self, window, cx);
pub(crate) fn close_editor(&mut self, window, cx);
pub(crate) fn choice_pick(&mut self, row: usize, window, cx);   // a click on a typeahead row
fn nudge(&mut self, steps: i64, window, cx);
fn sync_editor(&mut self, cx);                                   // mirror into the delegate
pub(crate) const MOVED: &str = "the cell moved; edit refused";

// popup.rs
pub(crate) struct ChoicePaint { pub rows: Vec<SharedString>, pub highlighted: usize }
pub(crate) fn choice_paint(list: &ChoiceList) -> ChoicePaint;
pub(crate) fn render_choice(p: &ChoicePaint, tile: &Entity<PricerTile>, cx: &App) -> impl IntoElement;

// delegate.rs
pub(crate) struct EditorPaint { pub row: usize, pub col: usize, pub input: Entity<InputState>, pub choice: Option<Rc<ChoicePaint>> }
// SheetDelegate fields: pub(crate) editor: Option<EditorPaint>, pub(crate) tile: WeakEntity<PricerTile>
// SheetDelegate::new(theme: &Theme, tile: WeakEntity<PricerTile>)
```

- [ ] **Step 1: Write the failing tests**

```rust
    // ---- Task 10 ----

    fn editor_text(h: &Harness, vcx: &VisualTestContext) -> Option<String> {
        h.tile.read_with(vcx, |t, cx| match &t.editor {
            Some(Editor::Text { input, .. } | Editor::Choice { input, .. }) => Some(input.read(cx).value().to_string()),
            None => None,
        })
    }

    fn set_editor(h: &Harness, vcx: &mut VisualTestContext, text: &str) {
        let text = text.to_string();
        vcx.update(|window, cx| {
            let input = match &h.tile.read(cx).editor {
                Some(Editor::Text { input, .. } | Editor::Choice { input, .. }) => input.clone(),
                None => panic!("an editor is open"),
            };
            // `set_value` emits no `Change` (CLAUDE.md's trap): every commit
            // path must re-read the live text, and this proves it does.
            input.update(cx, |s, cx| s.set_value(text.clone(), window, cx));
        });
    }

    /// Spec §12: editing the strike marks it stale and resubmits.
    #[gpui::test]
    fn i_on_a_strike_edits_it_and_enter_reprices_that_line(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_seeded(cx, &BOOK);
        let b = h.prices().remove(0);
        h.answer(&mut vcx, &b, 12.5);
        h.dispatch(&mut vcx, "right", Some(3)); // strike
        h.dispatch(&mut vcx, "edit", None);
        assert_eq!(h.mode(&mut vcx), "insert");
        assert_eq!(editor_text(&h, &vcx).as_deref(), Some("5000"));
        assert!(
            vcx.update(|window, cx| h.content.holds_focus(window, cx)),
            "the shell's insert-focus predicate sees the editor"
        );
        set_editor(&h, &mut vcx, "5100");
        h.dispatch(&mut vcx, "commit", None);
        assert_eq!(h.mode(&mut vcx), "normal");
        assert!(!vcx.update(|window, cx| h.content.holds_focus(window, cx)));
        assert_eq!(h.cell(&vcx, 0, "strike"), "5100");
        let again = h.prices();
        assert_eq!(again[0].lines.iter().map(|l| l.id).collect::<Vec<_>>(), vec![1]);
        assert!(h.tile.read_with(&vcx, |t, _| t.undo.can_undo()), "a cell commit is one undo entry");
    }

    #[gpui::test]
    fn a_bad_value_keeps_the_editor_open_with_the_reason(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_seeded(cx, &BOOK);
        h.dispatch(&mut vcx, "edit", None); // qty
        set_editor(&h, &mut vcx, "0");
        h.dispatch(&mut vcx, "commit", None);
        assert_eq!(h.mode(&mut vcx), "insert");
        assert_eq!(h.footer(&vcx).as_deref(), Some("quantity must not be zero"));
    }

    #[gpui::test]
    fn a_read_only_cell_and_a_package_row_say_so(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_seeded(cx, &BOOK);
        h.dispatch(&mut vcx, "last_col", None); // rho
        h.dispatch(&mut vcx, "edit", None);
        assert_eq!(h.mode(&mut vcx), "normal");
        assert_eq!(h.footer(&vcx).as_deref(), Some(crate::core::READ_ONLY));
        h.dispatch(&mut vcx, "first_col", None);
        h.dispatch(&mut vcx, "down", None); // the package
        h.dispatch(&mut vcx, "edit", None);
        assert_eq!(h.footer(&vcx).as_deref(), Some(crate::core::READ_ONLY));
    }

    #[gpui::test]
    fn up_and_down_nudge_by_the_texts_precision_and_shift_steps_ten(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_seeded(cx, &BOOK);
        h.dispatch(&mut vcx, "right", Some(3));
        h.dispatch(&mut vcx, "edit", None);
        h.dispatch(&mut vcx, "insert_up", None);
        assert_eq!(editor_text(&h, &vcx).as_deref(), Some("5001"));
        h.dispatch(&mut vcx, "insert_down_big", None);
        assert_eq!(editor_text(&h, &vcx).as_deref(), Some("4991"));
        h.dispatch(&mut vcx, "insert_up", Some(3));
        assert_eq!(editor_text(&h, &vcx).as_deref(), Some("4994"), "a count multiplies");
    }

    #[gpui::test]
    fn an_empty_shift_commit_inherits(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_seeded(cx, &BOOK);
        let spot = h.columns(&vcx).iter().position(|c| c == "spot_shift").unwrap();
        h.dispatch(&mut vcx, "right", Some(spot as u32));
        h.dispatch(&mut vcx, "edit", None);
        set_editor(&h, &mut vcx, "2");
        h.dispatch(&mut vcx, "commit", None);
        assert_eq!(h.tile.read_with(&vcx, |t, _| t.sheet.shift(0).spot_pct), Some(2.0));
        h.dispatch(&mut vcx, "edit", None);
        assert_eq!(editor_text(&h, &vcx).as_deref(), Some("2"));
        set_editor(&h, &mut vcx, "");
        h.dispatch(&mut vcx, "commit", None);
        assert_eq!(h.tile.read_with(&vcx, |t, _| t.sheet.shift(0).spot_pct), None, "empty means inherit");
    }

    #[gpui::test]
    fn a_type_cell_opens_a_typeahead_that_filters_and_enter_picks(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_seeded(cx, &BOOK);
        h.dispatch(&mut vcx, "right", Some(4)); // type
        h.dispatch(&mut vcx, "edit", None);
        assert_eq!(h.mode(&mut vcx), "insert");
        assert!(h.tile.read_with(&vcx, |t, _| matches!(t.editor, Some(Editor::Choice { .. }))));
        vcx.simulate_input("p");
        h.draw(&mut vcx);
        h.dispatch(&mut vcx, "commit", None);
        assert_eq!(h.cell(&vcx, 0, "type"), "P");
        // An unknown underlying commits as typed (planning decision 17).
        h.dispatch(&mut vcx, "first_col", None);
        h.dispatch(&mut vcx, "right", None); // underlying
        h.dispatch(&mut vcx, "edit", None);
        vcx.simulate_input("ndx");
        h.draw(&mut vcx);
        h.dispatch(&mut vcx, "commit", None);
        assert_eq!(h.cell(&vcx, 0, "underlying"), "NDX");
        // A closed vocabulary refuses a query nothing matches.
        h.dispatch(&mut vcx, "right", Some(3)); // type
        h.dispatch(&mut vcx, "edit", None);
        vcx.simulate_input("x");
        h.draw(&mut vcx);
        h.dispatch(&mut vcx, "commit", None);
        assert_eq!(h.footer(&vcx).as_deref(), Some("no option matches"));
        assert_eq!(h.mode(&mut vcx), "insert");
    }

    /// Spec §12 ("the editor blurs before it drops"), both closers.
    #[gpui::test]
    fn the_editor_gives_up_focus_before_it_is_dropped(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_seeded(cx, &BOOK);
        h.dispatch(&mut vcx, "right", Some(3));
        h.dispatch(&mut vcx, "edit", None);
        assert!(focused(&mut vcx));
        h.dispatch(&mut vcx, "cancel", None);
        assert!(!focused(&mut vcx), "cancel: blurred, then dropped");
        h.dispatch(&mut vcx, "edit", None);
        set_editor(&h, &mut vcx, "5100");
        h.dispatch(&mut vcx, "commit", None);
        assert!(!focused(&mut vcx), "commit: blurred, then dropped");
        h.dispatch(&mut vcx, "right", None); // type: the typeahead's field too
        h.dispatch(&mut vcx, "edit", None);
        assert!(focused(&mut vcx));
        h.dispatch(&mut vcx, "cancel", None);
        assert!(!focused(&mut vcx));
    }

    #[gpui::test]
    fn a_click_cancels_an_open_editor_and_never_commits(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_seeded(cx, &BOOK);
        h.dispatch(&mut vcx, "right", Some(3));
        h.dispatch(&mut vcx, "edit", None);
        set_editor(&h, &mut vcx, "5100");
        let at = centre_of(&mut vcx, "pricer-cell-2-2");
        click_at(&mut vcx, at, 1);
        h.draw(&mut vcx);
        assert_eq!(h.mode(&mut vcx), "normal");
        assert_eq!(h.cell(&vcx, 0, "strike"), "5000", "nothing was committed");
    }

    #[gpui::test]
    fn a_double_click_opens_the_editor_on_that_cell(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_seeded(cx, &BOOK);
        let at = centre_of(&mut vcx, "pricer-cell-2-4"); // B's strike
        click_at(&mut vcx, at, 1);
        click_at(&mut vcx, at, 2);
        h.draw(&mut vcx);
        assert_eq!(h.mode(&mut vcx), "insert");
        assert_eq!(editor_text(&h, &vcx).as_deref(), Some("4000"));
    }

    #[gpui::test]
    fn a_commit_whose_line_went_away_is_refused(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_seeded(cx, &BOOK);
        h.dispatch(&mut vcx, "right", Some(3));
        h.dispatch(&mut vcx, "edit", None);
        set_editor(&h, &mut vcx, "5100");
        h.tile.update(&mut vcx, |t, cx| t.apply_edit(Edit::Remove { at: 0 }, cx)).unwrap();
        h.dispatch(&mut vcx, "commit", None);
        assert_eq!(h.footer(&vcx).as_deref(), Some(MOVED));
        assert_eq!(h.mode(&mut vcx), "normal");
    }
```

- [ ] **Step 2: Run to see them fail**

Run: `cargo test -p geode-pricer tile::tests`
Expected: the new tests FAIL.

- [ ] **Step 3: `popup.rs` — the typeahead's paint**

```rust
//! The tile's popups (spec §8.4–§8.5): the choice typeahead under an
//! editing cell, and (Task 11) the action menu. Rows are prepared when
//! the list changes, never formatted in render.

use crate::tile::PricerTile;
use geode_shell::choice::ChoiceList;
use geode_shell::shell::scale;
use gpui::prelude::*;
use gpui::{App, Div, Entity, MouseButton, SharedString, div};
use gpui_component::{ActiveTheme as _, ThemeStyled as _, h_flex, v_flex};

const ROW_HEIGHT: f32 = 26.0;
const ROW_INSET: f32 = 8.0;
const MIN_WIDTH: f32 = 160.0;

pub(crate) fn popover_surface(cx: &App) -> Div {
    v_flex().min_w(scale::design(MIN_WIDTH)).p_1().gap_y_0p5().text_sm().popover_style(cx)
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ChoicePaint {
    pub rows: Vec<SharedString>,
    /// Window-relative, as `ChoiceList::highlighted` answers it.
    pub highlighted: usize,
}

pub(crate) fn choice_paint(list: &ChoiceList) -> ChoicePaint {
    ChoicePaint {
        rows: list.painted().iter().map(|r| list.options()[r.row].clone().into()).collect(),
        highlighted: list.highlighted(),
    }
}

/// The ranked options under the editing cell. A row click picks it
/// (`stop_propagation`: a click that means "pick" must not also land on
/// the grid, which would cancel the editor); a press anywhere else closes
/// the editor.
pub(crate) fn render_choice(p: &ChoicePaint, tile: &Entity<PricerTile>, cx: &App) -> impl IntoElement {
    let theme = cx.theme();
    let mut list = popover_surface(cx)
        .debug_selector(|| "pricer-choice".into())
        .occlude()
        .on_mouse_down_out({
            let tile = tile.clone();
            move |_, window, cx| tile.update(cx, |t, cx| t.close_editor(window, cx))
        });
    if p.rows.is_empty() {
        list = list.child(
            div()
                .h(scale::design(ROW_HEIGHT))
                .px(scale::design(ROW_INSET))
                .flex()
                .items_center()
                .text_color(theme.muted_foreground)
                .child("no option matches"),
        );
    }
    for (i, text) in p.rows.iter().enumerate() {
        list = list.child(
            h_flex()
                .h(scale::design(ROW_HEIGHT))
                .px(scale::design(ROW_INSET))
                .rounded(theme.radius)
                .items_center()
                .when(i == p.highlighted, |d| d.bg(theme.accent).text_color(theme.accent_foreground))
                .when(i != p.highlighted, |d| d.text_color(theme.popover_foreground))
                .debug_selector(move || format!("pricer-choice-row-{i}"))
                .on_mouse_down(MouseButton::Left, {
                    let tile = tile.clone();
                    move |_, window, cx| {
                        cx.stop_propagation();
                        tile.update(cx, |t, cx| t.choice_pick(i, window, cx))
                    }
                })
                .child(text.clone()),
        );
    }
    list
}
```

These are the market-data typeahead's tokens (`accent` on the highlighted row, `popover_foreground` elsewhere), which the shell's list-row sweep already covers; the pricer adds no new pair here.

- [ ] **Step 4: The editor in the tile**

```rust
pub(crate) const MOVED: &str = "the cell moved; edit refused";

/// The open cell editor (spec §8.4): its target by line identity and
/// column kind — re-checked at commit, so a line deleted or a view
/// switched under an open editor refuses rather than writing elsewhere.
pub(crate) enum Editor {
    Text { line: LineId, col: usize, kind: ColumnKind, input: Entity<InputState> },
    Choice { line: LineId, col: usize, kind: ColumnKind, input: Entity<InputState>, list: ChoiceList, free: bool },
}

impl Editor {
    fn input(&self) -> &Entity<InputState> {
        match self {
            Editor::Text { input, .. } | Editor::Choice { input, .. } => input,
        }
    }
    fn target(&self) -> (LineId, usize, ColumnKind) {
        match self {
            Editor::Text { line, col, kind, .. } | Editor::Choice { line, col, kind, .. } => (*line, *col, *kind),
        }
    }
}
```

Field `pub(crate) editor: Option<Editor>` (initialised `None`). `mode()`: `entry` if an entry is open, else `insert` if an editor is open, else `normal`. `holds_focus` also answers `true` when `self.editor`'s input is focused.

```rust
    /// `i`/`enter`/double-click (spec §8.4): a text field on the cell's
    /// grammar spelling, or a typeahead over its vocabulary; a cell that
    /// does not edit says why in the footer.
    fn begin_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.loading {
            self.footer = Some("the sheet is still loading".into());
            return;
        }
        self.close_editor(window, cx);
        let (Some(row), Some(planned)) = (self.cursor_sheet_row(), self.plan.columns.get(self.cursor.col)) else {
            return;
        };
        let (line, col, kind) = (self.sheet.id(row), self.cursor.col, planned.def.kind);
        let editor = match cell::editor_for(&self.sheet, row, kind) {
            Err(why) => {
                self.footer = Some(why.into());
                return;
            }
            Ok(CellEditor::Text(text)) => {
                let input = cx.new(|cx| InputState::new(window, cx));
                input.update(cx, |s, cx| s.set_value(text, window, cx));
                Editor::Text { line, col, kind, input }
            }
            Ok(CellEditor::Choice { options, current, free }) => {
                let input = cx.new(|cx| InputState::new(window, cx).placeholder(current.clone()));
                cx.subscribe_in(&input, window, |this, input, event: &InputEvent, _window, cx| {
                    if let InputEvent::Change = event {
                        let query = input.read(cx).value().to_string();
                        // The borrow of `list` must end before `sync_editor`.
                        let changed = match &mut this.editor {
                            Some(Editor::Choice { list, .. }) => list.set_query(&query),
                            _ => false,
                        };
                        if changed {
                            this.sync_editor(cx);
                        }
                    }
                })
                .detach();
                let mut list = ChoiceList::new(options, DEFAULT_CAP);
                list.place(Some(&current));
                Editor::Choice { line, col, kind, input, list, free }
            }
        };
        editor.input().read(cx).focus_handle(cx).focus(window, cx);
        self.editor = Some(editor);
        self.sync_editor(cx);
    }

    /// `enter`: the live text (re-read — `set_value` emits no `Change`),
    /// the target re-checked, parsed into one `Edit`; a bad value keeps
    /// the editor open. The editor closes (blur first) BEFORE the edit
    /// applies, so the rebuild never paints a dead field.
    fn commit_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // The editor's borrow ends inside this block, before any `self` call.
        let (value, (line, col, kind)) = {
            let Some(editor) = self.editor.as_mut() else {
                return;
            };
            let text = editor.input().read(cx).value().to_string();
            let target = editor.target();
            let value = match editor {
                Editor::Text { .. } => Some(text),
                Editor::Choice { list, free, .. } => {
                    list.set_query(&text);
                    match list.pick() {
                        Some(i) => Some(list.options()[i].clone()),
                        None if *free && !text.trim().is_empty() => Some(text),
                        None => None,
                    }
                }
            };
            (value, target)
        };
        let Some(value) = value else {
            self.footer = Some("no option matches".into());
            self.sync_editor(cx);
            self.rebuild_chrome();
            cx.notify();
            return;
        };
        let row = self.sheet.index_of(line);
        let same_column = self.plan.columns.get(col).is_some_and(|c| c.def.kind == kind);
        let Some(row) = row.filter(|_| same_column) else {
            self.close_editor(window, cx);
            self.footer = Some(MOVED.into());
            self.rebuild_chrome();
            cx.notify();
            return;
        };
        match cell::commit(&self.sheet, row, kind, &value) {
            Err(why) => {
                self.footer = Some(why.into());
                self.rebuild_chrome();
                cx.notify();
            }
            Ok(edit) => {
                self.close_editor(window, cx);
                if let Err(e) = self.apply_edit(edit, cx) {
                    self.footer = Some(e.to_string().into());
                    self.rebuild_chrome();
                    cx.notify();
                }
            }
        }
    }

    /// A click on a typeahead row: highlight it, then commit.
    pub(crate) fn choice_pick(&mut self, row: usize, window: &mut Window, cx: &mut Context<Self>) {
        let picked = match &mut self.editor {
            Some(Editor::Choice { list, input, .. }) => list
                .set_highlighted(row)
                .then(|| (input.clone(), list.highlighted_text().unwrap_or_default().to_string())),
            _ => None,
        };
        if let Some((input, text)) = picked {
            input.update(cx, |s, cx| s.set_value(text, window, cx));
            self.commit_edit(window, cx);
        }
    }

    /// Blur only if OUR field holds focus, then drop it.
    pub(crate) fn close_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(editor) = self.editor.take() else {
            return;
        };
        if editor.input().read(cx).focus_handle(cx).is_focused(window) {
            window.blur(cx);
        }
        self.sync_editor(cx);
        cx.notify();
    }

    /// `up`/`down` (`shift`: ten) in a numeric editor; in a typeahead they
    /// move the highlight.
    fn nudge(&mut self, steps: i64, window: &mut Window, cx: &mut Context<Self>) {
        let refused = match &mut self.editor {
            Some(Editor::Text { kind, input, .. }) => {
                let text = input.read(cx).value().to_string();
                match cell::nudge(*kind, &text, steps) {
                    Ok(next) => {
                        input.update(cx, |s, cx| s.set_value(next, window, cx));
                        None
                    }
                    Err(why) => Some(why),
                }
            }
            // `up` moves the highlight up the list: a negative step.
            Some(Editor::Choice { list, .. }) => {
                list.nav(NavCommand::Move(-steps));
                None
            }
            None => None,
        };
        if let Some(why) = refused {
            self.footer = Some(why.into());
        }
        self.sync_editor(cx);
    }

    /// Mirror the open editor into the delegate: its grid cell (looked up
    /// by line — a delivery can rebuild the model under it), its field,
    /// and the typeahead's prepared rows.
    fn sync_editor(&mut self, cx: &mut Context<Self>) {
        let paint = self.editor.as_ref().and_then(|e| {
            let (line, col, _) = e.target();
            let row = self.model.grid_row_of(line)?;
            let choice = match e {
                Editor::Choice { list, .. } => Some(Rc::new(choice_paint(list))),
                Editor::Text { .. } => None,
            };
            Some(EditorPaint { row, col, input: e.input().clone(), choice })
        });
        self.table.update(cx, |t, cx| {
            t.delegate_mut().editor = paint;
            cx.notify();
        });
    }
```

Call `self.sync_editor(cx)` at the end of `install_model` too (a rebuild moves grid rows).

In `dispatch`, extend the preface and the four arms:

```rust
        // Any verb but the field's own closes an open field first.
        let field_verb = matches!(
            verb,
            "commit" | "cancel" | "insert_up" | "insert_down" | "insert_up_big" | "insert_down_big"
        );
        if !field_verb {
            self.close_entry(window, cx);
            self.close_editor(window, cx);
        }
```

```rust
            "edit" => {
                self.begin_edit(window, cx);
                self.rebuild_chrome();
                cx.notify();
                return true;
            }
            "commit" => {
                if self.entry.is_some() {
                    self.commit_entry(window, cx);
                } else {
                    self.commit_edit(window, cx);
                }
                return true;
            }
            "cancel" => {
                self.close_entry(window, cx);
                self.close_editor(window, cx);
                return true;
            }
            "insert_up" | "insert_down" | "insert_up_big" | "insert_down_big" => {
                let up = verb.starts_with("insert_up");
                if self.entry.is_some() {
                    self.step_history(if up { 1 } else { -1 }, window, cx);
                } else {
                    let magnitude = if verb.ends_with("_big") { 10 } else { 1 };
                    let steps = (if up { magnitude } else { -magnitude }) * n as i64;
                    self.nudge(steps, window, cx);
                    self.rebuild_chrome();
                    cx.notify();
                }
                return true;
            }
```

(Replace Task 9's four arms with these.) In `on_table_event`: close the editor as well as the entry on `SelectCell` (a click anywhere cancels, never commits), and add

```rust
            TableEvent::DoubleClickedCell(row, col) => {
                self.close_entry(window, cx);
                self.set_cursor_row(*row);
                if let Some(c) = SheetDelegate::plan_col(*col) {
                    self.cursor.col = c;
                    self.sync_cursor(cx);
                    self.begin_edit(window, cx);
                    self.rebuild_chrome();
                    cx.notify();
                }
            }
```

Imports: `crate::core::cell::{self, CellEditor}`, `crate::core::columns::ColumnKind`, `crate::delegate::EditorPaint`, `crate::popup::choice_paint`, `geode_shell::choice::{ChoiceList, DEFAULT_CAP}`, `geode_shell::vimnav::NavCommand`, `gpui_component::input::{InputEvent, InputState}`.

- [ ] **Step 5: The delegate paints the field and the typeahead**

In `delegate.rs`:

```rust
/// A paint-time copy of the tile's open editor (`PricerTile::sync_editor`).
#[derive(Clone)]
pub(crate) struct EditorPaint {
    pub row: usize,
    pub col: usize,
    pub input: Entity<InputState>,
    pub choice: Option<Rc<ChoicePaint>>,
}
```

Add `pub(crate) editor: Option<EditorPaint>` and `pub(crate) tile: WeakEntity<PricerTile>` to `SheetDelegate`; `new` takes `(theme: &Theme, tile: WeakEntity<PricerTile>)`. In `PricerTile::new`, take `let weak = cx.weak_entity();` before building the table and pass it. In `render_td`'s value arm, when `self.editor` is at `(row_ix, plan_col)`, paint the field in place of the text, and hang the typeahead under the cell through `deferred` (so the table's clip never cuts it):

```rust
        let editing = self.editor.as_ref().filter(|e| e.row == row_ix && e.col == plan_col).cloned();
        let el = base
            .when(right, |el| el.justify_end())
            .when(at_cursor, |el| el.border_1().border_color(active_border));
        match editing {
            Some(e) => {
                let popup = e.choice.as_ref().and_then(|paint| {
                    let tile = self.tile.upgrade()?;
                    Some(gpui::deferred(render_choice(paint, &tile, cx)).with_priority(1))
                });
                el.child(div().flex_1().debug_selector(|| format!("pricer-editor-{row_ix}-{col_ix}")).child(Input::new(&e.input)))
                    .when_some(popup, |el, popup| el.relative().child(div().absolute().left_0().bottom_0().child(popup)))
                    .into_any_element()
            }
            None => el
                .when_some(row.cells.get(plan_col), |el, cell| {
                    el.text_color(paints.text(cell.state, package)).child(cell.text.clone())
                })
                .into_any_element(),
        }
```

If `deferred(..).with_priority` is not how the pinned gpui spells it, copy the market-data delegate's own anchoring around its `render_choice` call (`geode-marketdata/src/delegate.rs:455-485`), which is the working reference; the requirement is only that the list paints over the rows below the cell and is not clipped.

- [ ] **Step 6: Run the tests**

Run: `cargo test -p geode-pricer tile::tests`
Expected: PASS.

- [ ] **Step 7: Gate and commit**

Run: `cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`

```bash
git add crates/geode-pricer/src/
git commit -m "feat(pricer): insert mode — cell editor, choice typeahead, nudge, double-click

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---
### Task 11: Structural verbs, undo/redo, put, the action menu, and the rest of the `:` vocabulary

**Files:**
- Modify: `crates/geode-pricer/src/tile.rs`, `crates/geode-pricer/src/popup.rs`
- Test: `crates/geode-pricer/src/tile.rs` (`mod tests`)

**Interfaces:**
- Consumes: Task 3's `clip::{spec_of, put_place}`, Task 4's `UndoStack::{undo, redo, can_undo, can_redo}` and `commands::{Command, ShiftField}`, Task 8's `apply_edit`, `apply_edits`, `after_edit`, `reprice_all`, Task 6's `set_view`.
- Produces:

```rust
// tile.rs
fn landed_row(place: Place) -> usize;                              // the flat row an insert's first row lands on
fn delete_row(&mut self, cx) -> Result<(), String>;
fn history_step(&mut self, redo: bool, cx) -> Result<(), String>; // u / ctrl+r
fn put(&mut self, below: bool, cx) -> Result<(), String>;
fn move_row(&mut self, delta: isize, cx) -> Result<(), String>;
fn group(&mut self, count: usize, cx) -> Result<(), String>;
fn ungroup(&mut self, cx) -> Result<(), String>;
fn set_sheet_shift(&mut self, field: ShiftField, value: Option<f64>, cx) -> Result<(), String>;
fn set_spot(&mut self, underlying: Option<String>, level: Option<f64>, cx) -> Result<(), String>;
fn toggle_menu(&mut self, cx);
pub(crate) fn menu_pick(&mut self, index: usize, window, cx);
pub(crate) fn close_menu(&mut self, cx);
// field: pub(crate) menu: Option<Menu>

// popup.rs
pub(crate) enum MenuItem { Action { id: &'static str, title: &'static str, enabled: Result<(), &'static str> }, View { name: SharedString, label: SharedString } }
pub(crate) struct Menu { pub items: Vec<MenuItem>, pub highlighted: usize }
pub(crate) fn render_menu(m: &Menu, tile: &Entity<PricerTile>, cx: &App) -> impl IntoElement;
```

- [ ] **Step 1: Write the failing tests**

```rust
    // ---- Task 11 ----

    fn answer_all(h: &Harness, vcx: &mut VisualTestContext, price: f64) {
        for b in h.prices() {
            h.answer(vcx, &b, price);
        }
    }

    /// Spec §12: `dd` then `u` restores the row with its numbers and asks
    /// for nothing.
    #[gpui::test]
    fn dd_then_u_restores_the_row_with_its_numbers_and_no_request(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_seeded(cx, &BOOK);
        answer_all(&h, &mut vcx, 12.5);
        h.dispatch(&mut vcx, "bottom", None);
        h.dispatch(&mut vcx, "delete", None);
        assert_eq!(h.tree(&vcx).len(), 2);
        assert!(h.prices().is_empty(), "a removal changes no remaining request");
        h.dispatch(&mut vcx, "undo", None);
        assert_eq!(h.tree(&vcx).len(), 3);
        assert_eq!(h.cell(&vcx, 2, "price"), "12.50", "its last result came back with it");
        assert!(h.prices().is_empty(), "…so nothing is re-requested");
        h.dispatch(&mut vcx, "redo", None);
        assert_eq!(h.tree(&vcx).len(), 2);
        assert_eq!(h.dispatch(&mut vcx, "redo", None), true);
        assert_eq!(h.footer(&vcx).as_deref(), Some("nothing to redo"));
    }

    #[gpui::test]
    fn undo_of_a_strike_edit_restores_it_and_reprices(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_seeded(cx, &BOOK);
        answer_all(&h, &mut vcx, 12.5);
        let e = new_strike(&h, &vcx, 0, 5100.0);
        edit(&h, &mut vcx, e);
        answer_all(&h, &mut vcx, 13.0);
        h.dispatch(&mut vcx, "undo", None);
        assert_eq!(h.cell(&vcx, 0, "strike"), "5000");
        assert_eq!(h.prices().len(), 1, "an instrument change is a request change (spec §9.3)");
    }

    #[gpui::test]
    fn p_puts_the_yanked_row_with_fresh_ids_and_prices_it(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_seeded(cx, &BOOK);
        answer_all(&h, &mut vcx, 12.5);
        h.dispatch(&mut vcx, "yank_row", None);
        h.dispatch(&mut vcx, "bottom", None);
        h.dispatch(&mut vcx, "put_below", None);
        assert_eq!(h.tree(&vcx), vec![
            "SPX Z26 5000 C".to_string(),
            "-5 SPX Z26 4800/5200 CS".to_string(),
            "SPX Z26 4000 P".to_string(),
            "SPX Z26 5000 C".to_string(),
        ]);
        let ids = h.tile.read_with(&vcx, |t, _| (t.sheet.id(0), t.sheet.id(5)));
        assert_ne!(ids.0, ids.1, "a put takes fresh ids");
        assert_eq!(h.prices()[0].lines.len(), 1, "and asks for its own price");
        // A package put from a leg lands at a root boundary.
        h.dispatch(&mut vcx, "top", None);
        h.dispatch(&mut vcx, "down", None);
        h.dispatch(&mut vcx, "yank_row", None);
        h.dispatch(&mut vcx, "expand", None);
        h.dispatch(&mut vcx, "down", None); // first leg
        h.dispatch(&mut vcx, "put_above", None);
        let roots = h.tile.read_with(&vcx, |t, _| t.sheet.roots().count());
        assert_eq!(roots, 5);
    }

    #[gpui::test]
    fn shift_j_and_k_move_within_the_parent_and_off_the_end_is_refused(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_seeded(cx, &BOOK);
        h.dispatch(&mut vcx, "move_down", None);
        assert_eq!(h.tree(&vcx)[1], "SPX Z26 5000 C", "A hopped over the package");
        assert_eq!(h.cursor(&vcx).map(|c| c.0), Some(1), "the cursor follows its line");
        h.dispatch(&mut vcx, "move_down", Some(5));
        assert_eq!(h.footer(&vcx).as_deref(), Some("cannot move past the end"));
        h.dispatch(&mut vcx, "top", None);
        h.dispatch(&mut vcx, "move_down", None); // the package hops down
        assert_eq!(h.tree(&vcx)[1], "-5 SPX Z26 4800/5200 CS");
    }

    #[gpui::test]
    fn g_p_groups_roots_into_a_custom_package_and_g_u_ungroups(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_seeded(cx, &["SPX Z26 5000 C", "SPX Z26 4000 P", "SPX Z26 3000 P"]);
        answer_all(&h, &mut vcx, 1.0);
        h.dispatch(&mut vcx, "group", Some(2));
        assert_eq!(h.tree(&vcx)[0], "CUSTOM SPX Z26", "a custom package, opened");
        assert_eq!(h.tree(&vcx).len(), 4);
        assert_eq!(h.cell(&vcx, 0, "price"), "2.00", "its sum: two legs of 1.00");
        assert!(h.prices().is_empty(), "grouping changes no request");
        h.dispatch(&mut vcx, "down", None); // a leg: g u acts on its package
        h.dispatch(&mut vcx, "ungroup", None);
        assert_eq!(h.tile.read_with(&vcx, |t, _| t.sheet.roots().count()), 3);
        h.dispatch(&mut vcx, "group", Some(9));
        assert_eq!(h.footer(&vcx).as_deref(), Some("group needs a contiguous run of top-level lines"));
    }

    /// Spec §12: `:shift spot 2` reprices only the lines that inherit it.
    #[gpui::test]
    fn colon_shift_spot_reprices_only_inheriting_lines(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_seeded(cx, &BOOK);
        edit(&h, &mut vcx, Edit::SetShift { row: 0, shift: crate::core::OwnShifts { spot_pct: Some(1.0), vol_pts: None } });
        answer_all(&h, &mut vcx, 12.5);
        h.command(&mut vcx, "shift spot 2").unwrap();
        let b = h.prices().remove(0);
        assert_eq!(b.lines.iter().map(|l| l.id).collect::<Vec<_>>(), vec![3, 4, 5], "line 1 has its own spot shift");
        assert!(h.header(&vcx).contains(&"spot +2%".to_string()));
        h.command(&mut vcx, "shift spot clear").unwrap();
        assert!(!h.header(&vcx).iter().any(|t| t.starts_with("spot")));
    }

    #[gpui::test]
    fn colon_spot_rides_in_the_batch_and_clear_is_one_undo(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_seeded(cx, &BOOK);
        answer_all(&h, &mut vcx, 12.5);
        h.command(&mut vcx, "spot spx 5100").unwrap();
        let b = h.prices().remove(0);
        assert_eq!(b.overrides.spot.get("SPX"), Some(&5100.0));
        assert_eq!(b.lines.len(), 4, "every SPX line is restaled (spec §9.3)");
        h.command(&mut vcx, "spot ndx 18000").unwrap();
        answer_all(&h, &mut vcx, 12.5);
        h.command(&mut vcx, "spot clear").unwrap();
        assert!(h.tile.read_with(&vcx, |t, _| t.sheet.overrides().spot.is_empty()));
        h.dispatch(&mut vcx, "undo", None);
        assert_eq!(h.tile.read_with(&vcx, |t, _| t.sheet.overrides().spot.len()), 2, "one undo restores both");
    }

    #[gpui::test]
    fn the_menu_opens_steps_and_picks_and_a_disabled_row_says_why(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_seeded(cx, &BOOK);
        answer_all(&h, &mut vcx, 12.5);
        h.dispatch(&mut vcx, "menu", None);
        assert_eq!(h.mode(&mut vcx), "menu");
        // Rows: Price all, Group, Ungroup, Undo, Redo, Delete row, then views.
        h.dispatch(&mut vcx, "menu_down", Some(2)); // Ungroup: A is not in a package
        h.dispatch(&mut vcx, "menu_pick", None);
        assert_eq!(h.footer(&vcx).as_deref(), Some("not in a package"));
        assert_eq!(h.mode(&mut vcx), "menu", "a disabled row keeps the menu open");
        h.dispatch(&mut vcx, "menu_close", None);
        h.dispatch(&mut vcx, "menu", None);
        h.dispatch(&mut vcx, "menu_pick", None); // Price all
        assert_eq!(h.mode(&mut vcx), "normal");
        assert_eq!(h.prices()[0].lines.len(), 4);
        h.dispatch(&mut vcx, "menu", None);
        h.dispatch(&mut vcx, "menu_down", Some(7)); // the second view: barrier
        h.dispatch(&mut vcx, "menu_pick", None);
        assert!(h.columns(&vcx).contains(&"barrier".to_string()));
    }

    #[gpui::test]
    fn every_colon_command_leaves_the_frame_alone(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_seeded(cx, &BOOK);
        let lines = [
            "view barrier",
            "shift spot 2",
            "spot SPX 5100",
            "price",
            "refresh 10s",
            "group",
            "ungroup",
        ];
        for word in crate::core::commands::VERBS {
            assert!(
                lines.iter().any(|l| l.split_whitespace().next() == Some(word)),
                "no sweep line for `:{word}`"
            );
        }
        let before = h.frame.read_with(&vcx, |f, _| f.versions());
        for line in lines {
            assert!(crate::core::commands::parse(line).is_ok(), "`{line}` no longer parses");
            let _ = h.command(&mut vcx, line);
            let after = h.frame.read_with(&vcx, |f, _| f.versions());
            assert_eq!(
                (after.scope, after.grouping, after.as_of),
                (before.scope, before.grouping, before.as_of),
                "`:{line}` moved the frame"
            );
            let (level, overlay) =
                h.diagnostics.update(&mut vcx, |d, _| (d.take_pending_level(), d.take_pending_overlay_toggle()));
            assert!(level.is_none() && !overlay, "`:{line}` reached the app");
        }
    }
```

The menu test counts rows: `Price all`(0), `Group`(1), `Ungroup`(2), `Undo`(3), `Redo`(4), `Delete row`(5), `vanilla`(6), `barrier`(7). If `Group` at the first row of an ungrouped sheet is enabled (it is: A is a top-level line), `menu_down` over it is plain.

- [ ] **Step 2: Run to see them fail**

Run: `cargo test -p geode-pricer tile::tests`
Expected: the new tests FAIL.

- [ ] **Step 3: The verbs**

```rust
/// The flat row an insert at `place` puts its first row on.
fn landed_row(place: Place) -> usize {
    match place {
        Place::Root { at } => at,
        Place::Leg { package, leg } => package + 1 + leg,
    }
}
```

(Use `landed_row` in Task 9's `commit_entry` too, replacing its inline `match`.)

```rust
    /// `d d` (spec §8.5): no confirm — `u` is one key away. What was
    /// deleted is what `p` puts.
    fn delete_row(&mut self, cx: &mut Context<Self>) -> Result<(), String> {
        let row = self.cursor_sheet_row().ok_or("no row")?;
        let spec = spec_of(&self.sheet, row);
        self.apply_edit(Edit::Remove { at: row }, cx).map_err(|e| e.to_string())?;
        self.register = Some(spec);
        Ok(())
    }

    /// `u` / `ctrl+r`. A refused inverse clears the whole history
    /// (`UndoStack`'s rule) and says so.
    fn history_step(&mut self, redo: bool, cx: &mut Context<Self>) -> Result<(), String> {
        let stepped = if redo { self.undo.redo(&mut self.sheet) } else { self.undo.undo(&mut self.sheet) };
        match stepped {
            Ok(true) => {
                self.after_edit(cx);
                Ok(())
            }
            Ok(false) => Err(if redo { "nothing to redo" } else { "nothing to undo" }.into()),
            Err(e) => {
                self.after_edit(cx);
                Err(format!("{} failed ({e}); history cleared", if redo { "redo" } else { "undo" }))
            }
        }
    }

    /// `p` / `shift+p`: the register as fresh rows (fresh ids, fresh
    /// requests) where `put_place` says (planning decision 12).
    fn put(&mut self, below: bool, cx: &mut Context<Self>) -> Result<(), String> {
        let spec = self.register.clone().ok_or("nothing to put")?;
        let place = put_place(&self.sheet, self.cursor_sheet_row(), below, &spec);
        self.apply_edit(Edit::Insert { place, rows: vec![spec.clone()] }, cx).map_err(|e| e.to_string())?;
        let id = self.sheet.id(landed_row(place));
        if matches!(spec, RowSpec::Package { .. }) {
            self.expansion.set(id, true);
        }
        self.cursor.line = Some(id);
        self.rebuild(cx);
        Ok(())
    }

    /// `shift+j` / `shift+k`: within the parent; the cursor follows its
    /// line (it is keyed by id).
    fn move_row(&mut self, delta: isize, cx: &mut Context<Self>) -> Result<(), String> {
        let row = self.cursor_sheet_row().ok_or("no row")?;
        self.apply_edit(Edit::Move { row, delta }, cx).map_err(|e| e.to_string())
    }

    /// `g p` (count): the cursor row and the next `count − 1` roots become
    /// a custom package, opened.
    fn group(&mut self, count: usize, cx: &mut Context<Self>) -> Result<(), String> {
        let first = self.cursor_sheet_row().ok_or("no row")?;
        self.apply_edit(Edit::Group { first, count, template: Template::Custom, id: None }, cx)
            .map_err(|e| e.to_string())?;
        let id = self.sheet.id(first);
        self.expansion.set(id, true);
        self.cursor.line = Some(id);
        self.rebuild(cx);
        Ok(())
    }

    /// `g u`: the package under the cursor — on a leg, its package.
    fn ungroup(&mut self, cx: &mut Context<Self>) -> Result<(), String> {
        let row = self.cursor_sheet_row().ok_or("no row")?;
        let package = if self.sheet.is_package(row) {
            row
        } else {
            self.sheet.parent(row).ok_or("not in a package")?
        };
        self.apply_edit(Edit::Ungroup { row: package }, cx).map_err(|e| e.to_string())
    }

    fn set_sheet_shift(&mut self, field: ShiftField, value: Option<f64>, cx: &mut Context<Self>) -> Result<(), String> {
        let mut s = self.sheet.sheet_shift();
        match field {
            ShiftField::Spot => s.spot_pct = value,
            ShiftField::Vol => s.vol_pts = value,
        }
        self.apply_edit(Edit::SetSheetShift(s), cx).map_err(|e| e.to_string())
    }

    /// `:spot` (ruling 1): one underlying set or cleared, or every
    /// override cleared as ONE undo entry.
    fn set_spot(&mut self, underlying: Option<String>, level: Option<f64>, cx: &mut Context<Self>) -> Result<(), String> {
        let edits: Vec<Edit> = match underlying {
            Some(underlying) => vec![Edit::SetSpotOverride { underlying, level }],
            None => self
                .sheet
                .overrides()
                .spot
                .keys()
                .map(|u| Edit::SetSpotOverride { underlying: u.clone(), level: None })
                .collect(),
        };
        self.apply_edits(edits, cx).map_err(|e| e.to_string())
    }
```

In `dispatch`, add the arms (each a `Result` whose `Err` goes to the footer):

```rust
            "delete" | "undo" | "redo" | "put_below" | "put_above" | "move_down" | "move_up" | "group" | "ungroup" => {
                if self.loading {
                    self.footer = Some("the sheet is still loading".into());
                } else {
                    let result = match verb {
                        "delete" => self.delete_row(cx),
                        "undo" => self.history_step(false, cx),
                        "redo" => self.history_step(true, cx),
                        "put_below" => self.put(true, cx),
                        "put_above" => self.put(false, cx),
                        "move_down" => self.move_row(n as isize, cx),
                        "move_up" => self.move_row(-(n as isize), cx),
                        "group" => self.group(n, cx),
                        _ => self.ungroup(cx),
                    };
                    if let Err(why) = result {
                        self.footer = Some(why.into());
                    }
                }
            }
            "menu" => {
                self.toggle_menu(cx);
                return true;
            }
            "menu_down" | "menu_up" => {
                if let Some(m) = self.menu.as_mut() {
                    let len = m.items.len() as isize;
                    let step = if verb == "menu_down" { n as isize } else { -(n as isize) };
                    m.highlighted = (m.highlighted as isize + step).clamp(0, len - 1) as usize;
                }
            }
            "menu_pick" => {
                let at = self.menu.as_ref().map(|m| m.highlighted);
                if let Some(at) = at {
                    self.menu_pick(at, window, cx);
                }
                return true;
            }
            "menu_close" => {
                self.close_menu(cx);
                return true;
            }
```

and extend the preface so any verb but the menu's own closes an open menu: `if self.menu.is_some() && !verb.starts_with("menu") { self.menu = None; }`. `mode()` answers `menu` while `self.menu` is `Some` (after the entry and editor checks).

In `command`, replace the Task 8 catch-all:

```rust
            Command::Shift { field, value } => self.set_sheet_shift(field, value, cx),
            Command::Spot { underlying, level } => self.set_spot(underlying, level, cx),
            Command::Group(count) => self.group(count.unwrap_or(1), cx),
            Command::Ungroup => self.ungroup(cx),
```

- [ ] **Step 4: The menu**

In `popup.rs`:

```rust
/// One row of the action menu (spec §8.5's `.`; planning decision 22).
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum MenuItem {
    Action { id: &'static str, title: &'static str, enabled: Result<(), &'static str> },
    /// `label` is prepared when the menu opens (`view: barrier ✓` on the
    /// current one), never formatted per frame.
    View { name: SharedString, label: SharedString },
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Menu {
    pub items: Vec<MenuItem>,
    pub highlighted: usize,
}

pub(crate) fn render_menu(m: &Menu, tile: &Entity<PricerTile>, cx: &App) -> impl IntoElement {
    let theme = cx.theme();
    let mut list = popover_surface(cx)
        .debug_selector(|| "pricer-menu".into())
        .occlude()
        .on_mouse_down_out({
            let tile = tile.clone();
            move |_, _window, cx| tile.update(cx, |t, cx| t.close_menu(cx))
        });
    for (i, item) in m.items.iter().enumerate() {
        let (label, enabled): (SharedString, bool) = match item {
            MenuItem::Action { title, enabled, .. } => ((*title).into(), enabled.is_ok()),
            MenuItem::View { label, .. } => (label.clone(), true),
        };
        list = list.child(
            h_flex()
                .h(scale::design(ROW_HEIGHT))
                .px(scale::design(ROW_INSET))
                .rounded(theme.radius)
                .items_center()
                .when(i == m.highlighted, |d| d.bg(theme.accent).text_color(theme.accent_foreground))
                .when(i != m.highlighted && enabled, |d| d.text_color(theme.popover_foreground))
                .when(i != m.highlighted && !enabled, |d| d.text_color(theme.muted_foreground))
                .debug_selector(move || format!("pricer-menu-row-{i}"))
                .on_mouse_down(MouseButton::Left, {
                    let tile = tile.clone();
                    move |_, window, cx| {
                        cx.stop_propagation();
                        tile.update(cx, |t, cx| t.menu_pick(i, window, cx))
                    }
                })
                .child(label),
        );
    }
    list
}
```

In `tile.rs`:

```rust
    fn menu_items(&self) -> Vec<MenuItem> {
        let row = self.cursor_sheet_row();
        let root_line = row.is_some_and(|r| self.sheet.is_line(r) && self.sheet.parent(r).is_none());
        let packaged = row.is_some_and(|r| self.sheet.is_package(r) || self.sheet.parent(r).is_some());
        let mut items = vec![
            MenuItem::Action { id: "pricer::price", title: "Price all", enabled: Ok(()) },
            MenuItem::Action {
                id: "pricer::group",
                title: "Group",
                enabled: if root_line { Ok(()) } else { Err("group needs a top-level line") },
            },
            MenuItem::Action {
                id: "pricer::ungroup",
                title: "Ungroup",
                enabled: if packaged { Ok(()) } else { Err("not in a package") },
            },
            MenuItem::Action {
                id: "pricer::undo",
                title: "Undo",
                enabled: if self.undo.can_undo() { Ok(()) } else { Err("nothing to undo") },
            },
            MenuItem::Action {
                id: "pricer::redo",
                title: "Redo",
                enabled: if self.undo.can_redo() { Ok(()) } else { Err("nothing to redo") },
            },
            MenuItem::Action {
                id: "pricer::delete",
                title: "Delete row",
                enabled: if row.is_some() { Ok(()) } else { Err("no row") },
            },
        ];
        for name in self.shared.views.borrow().names() {
            let label = if name == self.sheet.view { format!("view: {name} ✓") } else { format!("view: {name}") };
            items.push(MenuItem::View { name: name.to_string().into(), label: label.into() });
        }
        items
    }

    fn toggle_menu(&mut self, cx: &mut Context<Self>) {
        self.menu = match self.menu {
            Some(_) => None,
            None => Some(Menu { items: self.menu_items(), highlighted: 0 }),
        };
        cx.notify();
    }

    pub(crate) fn close_menu(&mut self, cx: &mut Context<Self>) {
        if self.menu.take().is_some() {
            cx.notify();
        }
    }

    /// A disabled row says why and keeps the menu open; an enabled one
    /// closes it and dispatches through the same door a key would.
    pub(crate) fn menu_pick(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(item) = self.menu.as_ref().and_then(|m| m.items.get(index)).cloned() else {
            return;
        };
        match item {
            MenuItem::Action { enabled: Err(why), .. } => {
                self.footer = Some(why.into());
                self.rebuild_chrome();
                cx.notify();
            }
            MenuItem::Action { id, .. } => {
                self.menu = None;
                self.dispatch(&ActionId(id.to_string()), None, window, cx);
            }
            MenuItem::View { name, .. } => {
                self.menu = None;
                if let Err(why) = self.set_view(&name, cx) {
                    self.footer = Some(why.into());
                }
                cx.notify();
            }
        }
    }
```

Field `pub(crate) menu: Option<Menu>` (initialised `None`). In `Render`, anchor the menu under the header's right edge (the market-data arrangement):

```rust
        let tile = cx.entity();
        let header = div()
            .relative()
            .w_full()
            .child(header)
            .when_some(self.menu.as_ref(), |el, m| {
                el.child(
                    div()
                        .absolute()
                        .right_0()
                        .top(scale::design(header::HEADER_HEIGHT))
                        .child(render_menu(m, &tile, cx)),
                )
            });
```

Note the preface's "any verb but the menu's own closes it" means `menu_pick`'s own `self.dispatch` of the chosen action runs with the menu already `None`.

- [ ] **Step 5: Run the tests**

Run: `cargo test -p geode-pricer tile::tests`
Expected: PASS.

- [ ] **Step 6: Gate and commit**

Run: `cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`

```bash
git add crates/geode-pricer/src/
git commit -m "feat(pricer): delete, undo/redo, put, move, group, the action menu, :shift/:spot/:group

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---
### Task 12: Write-behind — the idle save, the refused-save notice, the flush on close

**Files:**
- Modify: `crates/geode-pricer/src/tile.rs`
- Test: `crates/geode-pricer/src/tile.rs` (`mod tests`)

**Interfaces:**
- Consumes: Task 5's `SheetStore::save`, `core::to_rows` (answers `None` for an empty sheet), Task 8's `after_edit`, Task 6's `set_view`, `on_release`.
- Produces: `pub(crate) const SAVE_IDLE: Duration = Duration::from_secs(1);`, `pub(crate) const NOT_SAVED: &str = "sheet not saved: the store refused it; the next edit retries";`, `fn arm_save(&mut self, cx)`, `pub(crate) fn save_now(&mut self)`, field `save_task: Option<Task<()>>`.

- [ ] **Step 1: Write the failing tests**

```rust
    // ---- Task 12 ----

    fn settle(vcx: &mut VisualTestContext, d: std::time::Duration) {
        vcx.executor().advance_clock(d);
        vcx.run_until_parked();
    }

    fn stored(h: &Harness) -> Sheet {
        crate::core::from_rows("book", &h.store.get("book").expect("a document")).unwrap()
    }

    #[gpui::test]
    fn an_edit_burst_saves_once_after_a_second_of_quiet(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_seeded(cx, &BOOK);
        let base = h.store.save_count();
        edit(&h, &mut vcx, Edit::SetQty { row: 0, qty: 2 });
        settle(&mut vcx, std::time::Duration::from_millis(500));
        edit(&h, &mut vcx, Edit::SetQty { row: 0, qty: 3 });
        settle(&mut vcx, std::time::Duration::from_millis(500));
        assert_eq!(h.store.save_count(), base, "a second edit inside the window re-arms it");
        settle(&mut vcx, SAVE_IDLE);
        assert_eq!(h.store.save_count(), base + 1, "one save for the burst");
        assert_eq!(stored(&h).qty(0), 3);
    }

    #[gpui::test]
    fn an_emptied_sheet_publishes_nothing_and_the_last_document_stays(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_seeded(cx, &["SPX Z26 5000 C"]);
        let base = h.store.save_count();
        h.dispatch(&mut vcx, "delete", None);
        settle(&mut vcx, SAVE_IDLE);
        assert_eq!(h.store.save_count(), base, "a zero-row document is never published (spec §7.2)");
        assert_eq!(stored(&h).len(), 1);
    }

    #[gpui::test]
    fn a_refused_save_notices_and_the_next_burst_retries(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_seeded(cx, &BOOK);
        h.store.set_refusing(true);
        edit(&h, &mut vcx, Edit::SetQty { row: 0, qty: 2 });
        settle(&mut vcx, SAVE_IDLE);
        assert_eq!(h.notice(&vcx).as_deref(), Some(NOT_SAVED));
        h.store.set_refusing(false);
        edit(&h, &mut vcx, Edit::SetQty { row: 0, qty: 4 });
        settle(&mut vcx, SAVE_IDLE);
        assert_eq!(h.notice(&vcx), None);
        assert_eq!(stored(&h).qty(0), 4);
    }

    #[gpui::test]
    fn view_and_refresh_changes_are_saved_with_the_sheet(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_seeded(cx, &BOOK);
        h.command(&mut vcx, "view barrier").unwrap();
        h.command(&mut vcx, "refresh off").unwrap();
        settle(&mut vcx, SAVE_IDLE);
        let s = stored(&h);
        assert_eq!(s.view, "barrier");
        assert_eq!(s.refresh, crate::core::Refresh::Off);
    }

    #[gpui::test]
    fn closing_flushes_a_pending_save_and_the_next_tile_reopens_it(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_seeded(cx, &BOOK);
        edit(&h, &mut vcx, Edit::SetQty { row: 0, qty: 9 });
        let (store, factory, frame, diagnostics) = (h.store.clone(), h.factory.clone(), h.frame.clone(), h.diagnostics.clone());
        drop(h);
        vcx.update(|window, _| window.remove_window());
        vcx.run_until_parked();
        drop(vcx);
        let saved = crate::core::from_rows("book", &store.get("book").unwrap()).unwrap();
        assert_eq!(saved.qty(0), 9, "the pending save ran on close");
        let mut record = toml::Table::new();
        record.insert("sheet".into(), "book".into());
        let title = cx.update(|cx| {
            let mut title = String::new();
            cx.open_window(gpui::WindowOptions::default(), |window, cx| {
                let o = factory.create(TileId(TILE + 3), Some(&record), frame.clone(), diagnostics.clone(), window, cx);
                title = o.content.title(cx).to_string();
                cx.new(|cx| gpui_component::Root::new(o.view, window, cx))
            })
            .unwrap();
            title
        });
        assert_eq!(title, "pricer · book", "the name was given back, so it opens under it");
    }
```

- [ ] **Step 2: Run to see them fail**

Run: `cargo test -p geode-pricer tile::tests`
Expected: the new tests FAIL (nothing saves).

- [ ] **Step 3: Implement**

```rust
pub(crate) const SAVE_IDLE: Duration = Duration::from_secs(1);
pub(crate) const NOT_SAVED: &str = "sheet not saved: the store refused it; the next edit retries";

    /// Spec §7.3: every change arms a one-second idle timer; a change
    /// inside the window re-arms it (replacing the task drops the old one).
    fn arm_save(&mut self, cx: &mut Context<Self>) {
        if self.loading {
            return;
        }
        self.save_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(SAVE_IDLE).await;
            let _ = this.update(cx, |t, cx| {
                t.save_task = None;
                t.save_now();
                t.rebuild_chrome();
                cx.notify();
            });
        }));
    }

    /// The whole sheet, once. An empty sheet publishes nothing (the last
    /// non-empty generation stays as history, spec §7.2); a refusal paints
    /// the header notice and the next burst retries.
    pub(crate) fn save_now(&mut self) {
        let Some(rows) = to_rows(&self.sheet) else {
            return;
        };
        if self.shared.store.save(&self.sheet.name, rows) {
            if self.notice.as_ref().is_some_and(|n| n.as_ref() == NOT_SAVED) {
                self.notice = None;
            }
        } else {
            self.notice = Some(NOT_SAVED.into());
        }
    }
```

Field `save_task: Option<Task<()>>` (initialised `None`). Call `self.arm_save(cx)` at the end of `after_edit` (every edit, undo and redo), in `set_view` after the rebuild, and in the `Command::Refresh` arm. Extend `on_release` so a pending save runs before the name is given back:

```rust
        cx.on_release(|this: &mut PricerTile, _cx| {
            // Spec §7.3: the sheet is not lost until the tile is — a save
            // still waiting on its idle timer runs now.
            if this.save_task.take().is_some() {
                this.save_now();
            }
            this.data.cancel(QueryKey(this.id.0));
            this.shared.open.borrow_mut().remove(&this.sheet.name);
        })
        .detach();
```

Import `crate::core::storage::to_rows`.

- [ ] **Step 4: Run the tests**

Run: `cargo test -p geode-pricer tile::tests`
Expected: PASS.

- [ ] **Step 5: Gate and commit**

Run: `cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`

```bash
git add crates/geode-pricer/src/tile.rs
git commit -m "feat(pricer): write-behind to the sheet store, flushed on close

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 13: Register the pricer in the app — roster, builtin views, `[pricing] refresh`, reload

**Files:**
- Modify: `Cargo.toml` (workspace `[workspace.dependencies]`), `crates/geode-app/Cargo.toml`
- Modify: `crates/geode-app/src/bridge.rs` (`DataSetup`, `data_setup`, `Bridge`, `start`, `attach`, two new readers, every test `Bridge { .. }` literal)
- Modify: `crates/geode-app/src/main.rs` (builtin layer, `init`, roster, `PricerFactoryHandle`)
- Test: `crates/geode-app/src/bridge.rs`, `crates/geode-app/src/main.rs` (`mod tests`)

**Interfaces:**
- Consumes: `geode_pricer::content::{PricerFactory, PricerSettings}`, `geode_pricer::store::MemorySheetStore`, `geode_pricer::core::{Views, PRICER_VIEWS_DOC, BUILTIN_VIEWS}`, `geode_pricer::init`, `geode_core::source_config::parse_duration`, `Frame::{versions, note_config_reloaded}`.
- Produces:

```rust
// bridge.rs
pub const DEFAULT_PRICING_REFRESH: Duration = Duration::from_secs(30);
pub fn pricing_refresh_from_config(config: &Config) -> (Option<Duration>, Option<Diagnostic>);
pub fn pricer_views_from_config(config: &Config) -> (Views, Vec<Diagnostic>);
pub struct DataSetup { .., pub pricer_views: Views, pub pricer_settings: PricerSettings }
pub struct Bridge { .., pub pricer: Rc<PricerFactory> }
// main.rs
fn builtin_layer(demo_root: Option<&Path>) -> Vec<LayerDoc>;
struct PricerFactoryHandle(Rc<PricerFactory>);
```

- [ ] **Step 1: Dependencies**

Root `Cargo.toml`, `[workspace.dependencies]`, after `geode-timeseries`: `geode-pricer = { path = "crates/geode-pricer" }`.

`crates/geode-app/Cargo.toml`, after the `geode-timeseries` entry:

```toml
# The line pricer (line-pricer spec §8): one roster factory, built beside
# the others in `bridge::start` because it needs the same `DataHandle`.
geode-pricer.workspace = true
```

- [ ] **Step 2: Write the failing bridge tests**

In `crates/geode-app/src/bridge.rs`'s `mod tests`:

```rust
    #[test]
    fn pricing_refresh_reads_off_a_duration_and_defaults_to_thirty_seconds() {
        let config = |text: &str| {
            Config::load(&ConfigSources {
                builtin: vec![LayerDoc::builtin("app", text).unwrap()],
                desk: None,
                user: None,
            })
        };
        assert_eq!(pricing_refresh_from_config(&config("")), (Some(DEFAULT_PRICING_REFRESH), None));
        assert_eq!(pricing_refresh_from_config(&config("[pricing]\nrefresh = \"off\"\n")), (None, None));
        assert_eq!(
            pricing_refresh_from_config(&config("[pricing]\nrefresh = \"10s\"\n")),
            (Some(Duration::from_secs(10)), None)
        );
        let (refresh, diag) = pricing_refresh_from_config(&config("[pricing]\nrefresh = \"soon\"\n"));
        assert_eq!(refresh, Some(DEFAULT_PRICING_REFRESH));
        let diag = diag.expect("a bad value warns");
        assert_eq!(diag.path.as_deref(), Some("app.pricing.refresh"));
    }

    #[test]
    fn pricer_views_fall_back_to_the_bundled_two_with_no_doc() {
        let config = Config::load(&ConfigSources { builtin: vec![], desk: None, user: None });
        let (views, diags) = pricer_views_from_config(&config);
        assert!(diags.is_empty());
        assert_eq!(views.names().collect::<Vec<_>>(), vec!["vanilla", "barrier"]);
    }

    /// Planning decision 20: a reload reaches the pricer through the
    /// frame's `config` counter — `ShellEvent::ConfigReloaded` never fires
    /// for a `pricer_views`-only edit.
    #[gpui::test]
    fn a_config_reload_hands_the_pricer_factory_its_views(cx: &mut gpui::TestAppContext) {
        let services = test_shell_services_with_sources(ConfigSources {
            builtin: vec![
                LayerDoc::builtin("views", "[tree]\ndataset = \"risk\"\n").unwrap(),
                LayerDoc::builtin("pricer_views", "[slim]\ncolumns = [\"qty\", \"price\"]\n").unwrap(),
            ],
            desk: None,
            user: None,
        });
        let window = open_test_window(cx, services);
        let mut vcx = gpui::VisualTestContext::from_window(window.into(), cx);
        vcx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        let (handle, _rx) = DataHandle::for_tests();
        let bridge = test_bridge(handle);
        assert_eq!(bridge.pricer.view_names(), vec!["vanilla", "barrier"], "fixture: built with the bundled views");
        cx.update(|cx| attach(&bridge, window, cx));
        let shell = window
            .root(&mut vcx)
            .unwrap()
            .read_with(&vcx, |root, _| root.view().clone().downcast::<ShellView>().unwrap());
        vcx.update(|_, cx| {
            let frame = shell.read(cx).frame().clone();
            frame.update(cx, |f, cx| {
                f.note_config_reloaded();
                cx.notify();
            });
        });
        vcx.run_until_parked();
        assert_eq!(bridge.pricer.view_names(), vec!["slim"]);
    }
```

`test_bridge(handle)` is a new test helper (Step 4) so the pricer field is added once rather than in every literal.

- [ ] **Step 3: Run to see them fail**

Run: `cargo test -p geode-app --bins pricing_refresh pricer_views a_config_reload_hands_the_pricer`
Expected: FAIL to compile.

- [ ] **Step 4: Implement the bridge**

The two readers, beside `stale_after_from_config`:

```rust
/// `[pricing] refresh` (spec §5.5, §9.4): the app default periodic
/// reprice. Absent is 30 s, `"off"` disables it, a duration is that; an
/// unreadable value keeps 30 s and says so (planning decision 21).
pub const DEFAULT_PRICING_REFRESH: Duration = Duration::from_secs(30);

pub fn pricing_refresh_from_config(config: &Config) -> (Option<Duration>, Option<Diagnostic>) {
    let Some(value) = config.get("app", "pricing.refresh") else {
        return (Some(DEFAULT_PRICING_REFRESH), None);
    };
    match value.as_str() {
        Some("off") => (None, None),
        Some(text) if let Some(d) = parse_duration(text).filter(|d| !d.is_zero()) => (Some(d), None),
        _ => (
            Some(DEFAULT_PRICING_REFRESH),
            Some(Diagnostic {
                severity: Severity::Warning,
                layer: config.explain("app", "pricing.refresh"),
                file: None,
                message: format!("[pricing] refresh = {value} is not a duration or \"off\"; using 30s"),
                path: Some("app.pricing.refresh".to_string()),
            }),
        ),
    }
}

/// The `pricer_views` doc, or the bundled two when no layer has one (the
/// builtin layer always does in the app; a test config may not).
pub fn pricer_views_from_config(config: &Config) -> (Views, Vec<Diagnostic>) {
    match config.doc(PRICER_VIEWS_DOC) {
        Some(doc) => Views::from_doc(doc),
        None => (Views::builtin(), Vec::new()),
    }
}
```

(`if let` guards in a match arm need the 2024 edition's let-chains in guards; if the pinned toolchain refuses, write the `Some(text)` arm with an inner `match parse_duration(..)`.)

`DataSetup` gains:

```rust
    /// The pricer's views and settings; `stale_after` is filled by `start`.
    pub pricer_views: Views,
    pub pricer_settings: PricerSettings,
```

In `data_setup`, after the pricer resolution:

```rust
    let (pricer_views, view_diags) = pricer_views_from_config(config);
    diagnostics.extend(view_diags);
    let (refresh, refresh_diag) = pricing_refresh_from_config(config);
    diagnostics.extend(refresh_diag);
    let pricer_settings = PricerSettings {
        pricer: pricer_name.clone(),
        pricer_missing: pricer.pricer.is_none(),
        refresh,
        stale_after: Duration::default(),
    };
```

(`pricer_name` is moved into the diagnostic's `format!` today; clone it before, or take the name from `pricer.name`.) Fill both fields in the `DataSetup` literal.

`Bridge` gains `pub pricer: Rc<geode_pricer::content::PricerFactory>`, documented: "The line pricer's factory, sharing the handle. Retained so a reload reaches its views and settings." In `start`:

```rust
    let mut pricer_settings = setup.pricer_settings.clone();
    pricer_settings.stale_after = stale_after;
    // Part 3's store is in-memory (line-pricer Part 3, planning decision
    // 8): a sheet survives closing and reopening a tile, not a restart.
    let pricer = Rc::new(PricerFactory::new(
        handle.clone(),
        Rc::new(MemorySheetStore::default()),
        setup.pricer_views.clone(),
        pricer_settings,
    ));
```

and `pricer` in the `Bridge` literal.

In `attach`, after the existing `ConfigReloaded` subscription:

```rust
    // The pricer refreshes on EVERY applied reload (planning decision 20):
    // `ShellEvent::ConfigReloaded` fires only for five named docs, and a
    // `pricer_views` or `[pricing] refresh` edit is neither. The frame's
    // `config` counter is the ungated signal (`main.rs`'s diagnostics
    // factory observes it the same way).
    {
        let pricer = bridge.pricer.clone();
        let diagnostics = diagnostics.clone();
        let shell = shell.clone();
        let frame = shell.read(cx).frame().clone();
        let last = Rc::new(Cell::new(frame.read(cx).versions().config));
        cx.observe(&frame, move |frame, cx| {
            let now = frame.read(cx).versions().config;
            if now == last.get() {
                return;
            }
            last.set(now);
            // Read everything out of the config before the factory takes
            // `cx` mutably.
            let (views, mut diags, refresh, stale_after) = {
                let config = shell.read(cx).config();
                let (views, diags) = pricer_views_from_config(config);
                let (refresh, refresh_diag) = pricing_refresh_from_config(config);
                let mut diags = diags;
                diags.extend(refresh_diag);
                (views, diags, refresh, stale_after_from_config(config))
            };
            pricer.reload(views, refresh, stale_after, cx);
            for d in &diags {
                tracing::warn!(target: "geode::pricing", "{d}");
            }
            if !diags.is_empty() {
                diagnostics.update(cx, |dg, cx| {
                    let before = dg.version();
                    dg.note_data_diagnostics(std::mem::take(&mut diags), SystemTime::now());
                    if dg.version() != before {
                        cx.notify();
                    }
                });
            }
        })
        .detach();
    }
```

Match the local names `attach` already binds (`shell`, `diagnostics`); if `diagnostics` is bound only inside the `ConfigReloaded` closure's clone list, clone it for this block too. Imports: `geode_pricer::content::{PricerFactory, PricerSettings}`, `geode_pricer::store::MemorySheetStore`, `geode_pricer::core::{PRICER_VIEWS_DOC, Views}`, `geode_core::source_config::parse_duration`, `std::cell::Cell`.

Add the test helper and use it in every `Bridge { .. }` literal in `mod tests` (there are about a dozen; each gains one line `pricer: test_pricer(&handle),` placed before `handle,` moves the handle):

```rust
    fn test_pricer(handle: &DataHandle) -> Rc<PricerFactory> {
        Rc::new(PricerFactory::new(
            handle.clone(),
            Rc::new(MemorySheetStore::default()),
            Views::builtin(),
            PricerSettings::default(),
        ))
    }

    /// A bridge whose every factory is a default; for tests that exercise
    /// one factory's reload path.
    fn test_bridge(handle: DataHandle) -> Bridge {
        let (_tx, rx) = crate::events::channel();
        Bridge {
            factory: Rc::new(BlotterFactory::new(
                handle.clone(),
                Vec::new(),
                NamedColours::default(),
                SchemaSpec::default(),
                DerivedDimensions::default(),
                FindStyle::default(),
                Duration::from_secs(900),
            )),
            marketdata: Rc::new(MarketDataFactory::new(handle.clone(), &CVI, Duration::from_secs(900))),
            dividend: Rc::new(MarketDataFactory::new(handle.clone(), &DIVIDEND, Duration::from_secs(900)).without_keymap()),
            timeseries: Rc::new(geode_timeseries::content::TimeseriesFactory::new(handle.clone(), NamedColours::default())),
            pricer: test_pricer(&handle),
            handle,
            events: rx,
            dropped: Arc::new(AtomicU64::new(0)),
            sources: Vec::new(),
            local_datasets: Default::default(),
        }
    }
```

If a literal's `_tx` must stay alive for its test (the catalog tests send on it), keep that test's own literal and add only the `pricer` line.

- [ ] **Step 5: Write the failing `main.rs` tests**

```rust
    #[test]
    fn the_builtin_layer_carries_the_two_pricer_views() {
        let builtin = builtin_layer(None);
        let config = Config::load(&ConfigSources { builtin, desk: None, user: None });
        let (views, diags) = geode_pricer::core::Views::from_doc(config.doc("pricer_views").expect("the doc"));
        assert!(diags.is_empty(), "{diags:?}");
        assert_eq!(views.names().collect::<Vec<_>>(), vec!["vanilla", "barrier"]);
    }

    #[test]
    fn the_roster_lists_the_pricer_and_registers_its_add_action() {
        use geode_data::DataHandle;
        use geode_shell::actions::ActionId;

        let mut roster = ModuleRoster::new();
        let (data, _rx) = DataHandle::for_tests();
        roster.add(Box::new(PricerFactoryHandle(Rc::new(geode_pricer::content::PricerFactory::new(
            data,
            Rc::new(geode_pricer::store::MemorySheetStore::default()),
            geode_pricer::core::Views::builtin(),
            geode_pricer::content::PricerSettings::default(),
        )))));
        assert!(roster.kinds().contains(&"pricer"));
        let mut registry = ActionRegistry::default();
        register_add_actions(&mut registry, &roster.kinds());
        roster.register_actions(&mut registry);
        assert_eq!(
            registry.get(&ActionId("tile::add_pricer".to_string())).expect("an add-tile row").title,
            "Pricer: Split"
        );
        assert!(registry.get(&ActionId("pricer::add_below".to_string())).is_some());
        let (docs, diags) = roster.keymap_fragments();
        assert!(diags.is_empty(), "{diags:?}");
        assert_eq!(docs.len(), 1);
    }
```

The expected add-tile title follows `register_add_actions`' own rule (`"Timeseries: Split"` for `timeseries`); if it capitalises differently, match the timeseries test's spelling rule, not this string.

- [ ] **Step 6: Implement `main.rs`**

Extract the builtin list at `main.rs:753-758` into a function and add the pricer's views:

```rust
/// Every builtin config doc: the shell's keymap, the pricer's two bundled
/// views (line-pricer Part 2, planning decision 12 — a desk or user layer
/// overrides a view by name), and the `--demo` layer.
fn builtin_layer(demo_root: Option<&Path>) -> Vec<LayerDoc> {
    let mut builtin = vec![
        LayerDoc::builtin("keymap", BUILTIN_KEYMAP).expect("builtin keymap TOML is well-formed"),
        LayerDoc::builtin(geode_pricer::core::PRICER_VIEWS_DOC, geode_pricer::core::BUILTIN_VIEWS)
            .expect("BUILTIN_VIEWS is well-formed TOML"),
    ];
    if let Some(root) = demo_root {
        builtin.extend(demo::layer(&root.join("src")));
    }
    builtin
}
```

and call it where the vector was built (`let builtin = builtin_layer(demo_root.as_deref());` — match `demo_root`'s actual type). Call `geode_pricer::init(cx);` beside `geode_marketdata::init(cx)` (`main.rs:122`). Add the forwarder beside `TimeseriesFactoryHandle`, forwarding every method (the doc on `MarketDataFactoryHandle` says why):

```rust
/// Same shape as [`TimeseriesFactoryHandle`], for the line pricer: the
/// bridge keeps a clone for its reload.
struct PricerFactoryHandle(Rc<geode_pricer::content::PricerFactory>);

impl ModuleFactory for PricerFactoryHandle {
    fn kind(&self) -> &'static str {
        self.0.kind()
    }
    fn register_actions(&self, registry: &mut ActionRegistry) {
        self.0.register_actions(registry)
    }
    fn contexts(&self) -> Vec<&'static str> {
        self.0.contexts()
    }
    fn default_keymap(&self) -> Option<&'static str> {
        self.0.default_keymap()
    }
    fn create(
        &self,
        tile: TileId,
        restored: Option<&toml::Table>,
        frame: Entity<Frame>,
        diagnostics: Entity<Diagnostics>,
        window: &mut Window,
        cx: &mut App,
    ) -> TileOccupant {
        self.0.create(tile, restored, frame, diagnostics, window, cx)
    }
}
```

and register it with the others: `roster.add(Box::new(PricerFactoryHandle(bridge.pricer.clone())));`.

- [ ] **Step 7: Run the app tests**

Run: `cargo test -p geode-app --bins`
Expected: PASS, including the four new tests and every existing bridge test with its new `pricer` field.

- [ ] **Step 8: Run the demo once**

Run: `cargo run -p geode-app -- --demo` and add a pricer tile (`mod+n`, "Pricer: Split"), type `o`, `-5 SPX Z26 95%/105% CS`, `enter`, and watch the row price. Note what you saw (numbers land, header count clears) in the task report; this is a smoke check, not the display check of record (Task 14 lists those for Matthew). If the demo's `$TMPDIR/geode-demo/<rows>-42/` predates a schema change and refuses to open, delete it (CLAUDE.md).

- [ ] **Step 9: Gate and commit**

Run: `cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace && cargo check -p geode-shell --features test-support --all-targets`

```bash
git add Cargo.toml Cargo.lock crates/geode-app/
git commit -m "feat(app): register the line pricer — roster, bundled views, [pricing] refresh, reload

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---
### Task 14: Harness entries, docs, performance record, spec as-built

**Files:**
- Modify: `scripts/mutation-check.sh` (append before `if (( anchors_only ))`)
- Modify: `docs/current/features.md` ("Pricing and the line pricer"), `docs/current/configuration.md` (`[pricing] refresh`), `docs/current/performance.md` (reference row), `docs/perf.md` (measurement log), `docs/phase-history.md` (one paragraph), `crates/geode-pricer/README.md`, `docs/superpowers/specs/2026-09-19-geode-line-pricer-design.md` (§18)

- [ ] **Step 1: Harness entries**

Append to `scripts/mutation-check.sh`, before `if (( anchors_only ))`. Each `from` must match its file exactly once; the anchors below are the code as this plan writes it — where an implementer's committed text differs (formatting, a renamed local), re-anchor on the committed line and keep the mutation's meaning. Verify with `zsh scripts/mutation-check.sh --anchors-only` (exit 0), then run the block: `zsh scripts/mutation-check.sh "pricer tile"`, `"pricer undo"`, `"pricer entry"`, `"pricer cell"`, `"pricer core: a tick"`, `"pricer app"`. Every entry must be CAUGHT; a survivor means the named test does not pin what it claims — strengthen the test, never delete the entry.

```bash
# Line pricer Part 3 (tile): the repricing rules, the two inputs'
# blur-then-drop, write-behind, the undo stack, placement, the reload.
run_mutation "pricer tile: an older submission's outcome is installed" \
  crates/geode-pricer/src/tile.rs \
  '        if outcome.key != QueryKey(self.id.0) || outcome.tag != self.tag {' \
  '        if outcome.key != QueryKey(self.id.0) {' \
  geode-pricer an_older_submissions_outcome_is_dropped_whole

run_mutation "pricer tile: a batch carries only the lines not in flight" \
  crates/geode-pricer/src/tile.rs \
  '        let lines: Vec<PriceLine> = stale
            .iter()
            .filter_map(|r| {' \
  '        let lines: Vec<PriceLine> = stale
            .iter()
            .filter(|r| self.in_flight.get(&self.sheet.id(**r)) != Some(&self.sheet.revision(**r)))
            .filter_map(|r| {' \
  geode-pricer an_older_submissions_outcome_is_dropped_whole

run_mutation "pricer tile: a hidden tile still submits" \
  crates/geode-pricer/src/tile.rs \
  '        if !self.visible || self.loading {' \
  '        if self.loading {' \
  geode-pricer hide_cancels_by_key_and_prices_nothing_until_shown

run_mutation "pricer tile: a hide does not cancel by key" \
  crates/geode-pricer/src/tile.rs \
  '            self.data.cancel(QueryKey(self.id.0));
            self.in_flight.clear();
            self.refresh_task = None;' \
  '            self.in_flight.clear();
            self.refresh_task = None;' \
  geode-pricer hide_cancels_by_key_and_prices_nothing_until_shown

run_mutation "pricer tile: a refused submission never retries" \
  crates/geode-pricer/src/tile.rs \
  '                t.retry_task = None;
                t.submit(cx);' \
  '                t.retry_task = None;' \
  geode-pricer a_refused_submission_notices_and_retries_after_a_second

run_mutation "pricer tile: a tick does not stale the sheet" \
  crates/geode-pricer/src/tile.rs \
  '        if self.sheet.is_empty() || self.loading {
            return;
        }
        self.sheet.mark_all_stale();' \
  '        if self.sheet.is_empty() || self.loading {
            return;
        }' \
  geode-pricer the_refresh_tick_marks_every_line_stale_and_submits

run_mutation "pricer tile: the flip barrier waits for the pricer" \
  crates/geode-pricer/src/tile.rs \
  '                    if f.arrived(key, now) {' \
  '                    if false && f.arrived(key, now) {' \
  geode-pricer the_tile_answers_a_flip_barrier_it_has_nothing_coming_for

run_mutation "pricer tile: the entry field is dropped unblurred" \
  crates/geode-pricer/src/tile.rs \
  '        if entry.input.read(cx).focus_handle(cx).is_focused(window) {' \
  '        if false && entry.input.read(cx).focus_handle(cx).is_focused(window) {' \
  geode-pricer escape_removes_the_placeholder_and_the_field_blurs_before_it_drops

run_mutation "pricer tile: the cell editor is dropped unblurred" \
  crates/geode-pricer/src/tile.rs \
  '        if editor.input().read(cx).focus_handle(cx).is_focused(window) {' \
  '        if false && editor.input().read(cx).focus_handle(cx).is_focused(window) {' \
  geode-pricer the_editor_gives_up_focus_before_it_is_dropped

run_mutation "pricer tile: a commit ignores that its line went away" \
  crates/geode-pricer/src/tile.rs \
  '        let Some(row) = row.filter(|_| same_column) else {' \
  '        let Some(row) = row.or(Some(0)).filter(|_| same_column) else {' \
  geode-pricer a_commit_whose_line_went_away_is_refused

run_mutation "pricer tile: a refused save is silent" \
  crates/geode-pricer/src/tile.rs \
  '            self.notice = Some(NOT_SAVED.into());' \
  '            let _ = NOT_SAVED;' \
  geode-pricer a_refused_save_notices_and_the_next_burst_retries

run_mutation "pricer tile: a close drops a pending save" \
  crates/geode-pricer/src/tile.rs \
  '            if this.save_task.take().is_some() {
                this.save_now();
            }' \
  '            let _ = this.save_task.take();' \
  geode-pricer closing_flushes_a_pending_save_and_the_next_tile_reopens_it

run_mutation "pricer core: a tick stales no line" \
  crates/geode-pricer/src/core/sheet.rs \
  '            if self.is_line(row) {
                self.state[row] = LineState::Stale;' \
  '            if false {
                self.state[row] = LineState::Stale;' \
  geode-pricer mark_all_stale_stales_every_line_and_bumps_no_revision

run_mutation "pricer undo: a fresh edit keeps the redo side" \
  crates/geode-pricer/src/core/undo.rs \
  '        self.undone.clear();
        self.done.push_back(undo);' \
  '        self.done.push_back(undo);' \
  geode-pricer a_new_edit_clears_the_redo_side

run_mutation "pricer undo: a refused inverse keeps a broken history" \
  crates/geode-pricer/src/core/undo.rs \
  '            Ok(redo) => {
                self.undone.push(redo);
                Ok(true)
            }
            Err(e) => {
                self.clear();' \
  '            Ok(redo) => {
                self.undone.push(redo);
                Ok(true)
            }
            Err(e) => {' \
  geode-pricer an_inverse_refused_mid_undo_clears_both_sides

run_mutation "pricer entry: o below a leg lands before it" \
  crates/geode-pricer/src/core/entry.rs \
  '            leg: if below { leg + 1 } else { leg },' \
  '            leg: if below { leg } else { leg },' \
  geode-pricer o_lands_after_the_cursor_row_and_shift_o_before_it

run_mutation "pricer cell: an empty shift commits zero" \
  crates/geode-pricer/src/core/cell.rs \
  '    if t.is_empty() {
        return Ok(None);
    }' \
  '    if t.is_empty() {
        return Ok(Some(0.0));
    }' \
  geode-pricer an_empty_shift_commit_inherits_and_a_signed_number_is_owned

run_mutation "pricer app: a config reload never reaches the pricer" \
  crates/geode-app/src/bridge.rs \
  '            pricer.reload(views, refresh, stale_after, cx);' \
  '            let _ = (views, refresh, stale_after);' \
  geode-app a_config_reload_hands_the_pricer_factory_its_views
```

Update the harness count wherever the script or its docs state one (`grep -c '^run_mutation "' scripts/mutation-check.sh`).

- [ ] **Step 2: Current docs**

`docs/current/features.md`, "Pricing and the line pricer": replace the paragraph that says the crate "has no tile and is not registered" with what is now true, in the current-guide voice (behaviour, constraint, failure semantics, limitation — no task chronology):

- the tile (kind `pricer`): sheet rows as lines and packages; `o`/`shift+o` shorthand entry with history; `i`/`enter`/double-click cell editing with typeahead for underlying, type and barrier type; `d d`, `u`/`ctrl+r` (100 entries, LIFO), `p`/`shift+p`, `shift+j`/`shift+k`, `g p`/`g u`, the tree keys, `y y`/`y c` (and why `y` alone is unbound), `.` menu;
- the `:` verbs (`view`, `shift`, `spot`, `price`, `refresh`, `group`, `ungroup`; `e`/`name`/`new`/`rm` refuse as not built);
- repricing: one batch per submission carrying every stale line; the older-tag drop; revision rule; hide cancels by key; the refresh timer (`[pricing] refresh`, per-sheet `:refresh`); a refused submission retries after a second; the tile arrives at flip barriers itself;
- persistence: write-behind after one idle second, flushed on close; an empty sheet publishes nothing; **known limitation: the store is in-memory until the DuckDB store lands — sheets survive closing a tile, not a restart; a restored name with no document opens empty with a notice**;
- known gaps: no catalogue underlyings in the typeahead; result cells are not sign-coloured; column widths are fixed pixels.

`docs/current/configuration.md`: where `[pricing]` is described (or beside `pricer_views` at line 25 if nowhere), add `refresh` — default `30s`, `"off"` disables, a bad value warns at `app.pricing.refresh` and keeps 30 s, applies on reload without restart (only `adapter` needs a restart).

`docs/current/performance.md`: add a reference row `| Line-pricer grid build | 1,000 rows, every package open | <median> |` from Task 5's bench, and one line under the cache/allocation contracts: "A pricer grid model is rebuilt on edit, delivery, expansion, view, clock or entry change, never in render; paints are a per-theme memo."

`docs/perf.md` (the measurement log): a `## Line pricer tile (spec §12, Part 3)` section in the Part 2 section's format — command, hardware, fixture (1,000 rows, every tenth a two-leg callspread, every line answered, every package open), the `grid_build_1000` median, and the 8 ms budget it is measured against.

`docs/phase-history.md`: one paragraph — Part 3 put the pricer on screen against an in-memory store; the decisions that matter later (the batch-carries-every-stale-line rule, the barrier self-arrival, the frame-counter reload, `nudge_text` moving to `geode-core`).

`crates/geode-pricer/README.md`: the summary now says it is the line-pricer module (core plus tile), registered in the roster; the layout table gains `cell`, `entry`, `clip`, `tree`, `undo`, `commands` (core) and `store`, `grid`, `paint`, `delegate`, `header`, `popup`, `session`, `content`, `tile`; "Rules this crate pins" gains: every mutation through `apply_edit` (the undo stack is strictly LIFO); a batch carries every stale line and an older tag is dropped whole; the grid model is built on change and installed through `install_model` only; both inputs blur before they drop and a click cancels an open editor; the tile arrives at flip barriers itself; an empty sheet is never saved.

Run `grep -rn 'no tile\|not registered' docs/current crates/geode-pricer/README.md` afterwards — nothing may still say the pricer has no tile.

- [ ] **Step 3: Spec as-built**

Append `## 18. As built (Part 3, <date>)` to the spec: the twenty-three planning decisions above in one line each (flag the three marked **(flag)**), any execution deviations found in review, the harness entry count, the `grid_build_1000` median, and "Part 4 obligations": the DuckDB `SheetStore` answering `Loaded::Pending` and the `Delivery::Query` arm calling `PricerTile::loaded`; `:e`/`:name`/`:new`/`:rm` (the open-name set exists: refuse an open name); the `pricer_sheets` declaration into the builtin layer (`builtin_layer` in `main.rs` is the place); `keep_generations` for local datasets; and the catalogue underlyings for the typeahead if a source for them exists by then.

- [ ] **Step 4: Final gate**

Run, in order: `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`, `cargo check -p geode-shell --features test-support --all-targets`, `zsh scripts/mutation-check.sh --anchors-only`.
Expected: all pass; anchors report no stale or duplicate entry.

- [ ] **Step 5: Commit**

```bash
git add scripts/mutation-check.sh docs/ crates/geode-pricer/README.md
git commit -m "docs(pricer): tile in the current guides, perf record, harness entries, spec as-built

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

- [ ] **Step 6: Display checks for Matthew (not automatable headlessly)**

List these in the handoff; none can be proven in `TestAppContext`:
1. The header row: name, view, shift chips, `N pricing…`, pricer name, time turning to `stale`.
2. The tree column: indent, chevrons, a package row's `secondary` ground on a light and a dark theme.
3. Stale (muted) versus fresh (foreground) result cells reading as different on at least three themes — the floor can pull `muted` close to `foreground`.
4. The entry field painted in the tree column over the active-row ground, and the next placeholder opening below after `enter`.
5. The cell editor in place, and the typeahead hanging under its cell unclipped at the bottom of the table.
6. The `.` menu under the header's right edge; a disabled row's muted text.
7. The footer holding its height when empty (the table must not jump when a message appears).

---

## Self-review notes

- **Spec coverage.** §8.1 factory/content/delegate/`init` → Task 6. §8.2 grid model, tree column, chevron, paints, sweep → Tasks 5–7. §8.3 header/footer → Tasks 6, 8. §8.4 modes, entry, insert, typeahead, nudge, blur-then-drop, click cancels → Tasks 9–10. §8.5 every key → Tasks 7, 9–11 (with `y` alone unbound, decision 16). §8.6 commands → Tasks 6, 8, 11 (`:e`/`:name`/`:new`/`:rm` refuse, decision 9). §9.1–§9.5 → Task 8 (decision 4 amends §9.1). §7.3 write-behind, §7.4 naming/restore/open set → Tasks 6, 12 (decision 8 brings them forward). §10.1 taxonomy → footer (user errors), header notices (data problems), `tracing` (bugs, decision 19). §12's tile list: `o`+line+`enter` (Task 9), delivery paints (Task 8), strike edit restales (Task 10), older revision ignored (Task 8), `dd`+`u` (Task 11), `:shift spot` (Task 11), tick (Task 8), restore requests and reprices (Tasks 6, 8), hide cancels (Task 8), editor blurs (Tasks 9–10), other key ignored (Task 8), theme sweep (Task 5). "`:e` refuses an open name" is Part 4's (the verb itself is Part 4's); the open-name set it will read is built and tested here. §17's Part 3 obligations: `deliver_all` (Task 8), strict LIFO (Task 4 + global constraint), `BUILTIN_VIEWS` into the builtin layer (Task 13), grid model on delivery not per frame (Tasks 5–6), package hopping upward and multi-edit redo tests (Task 4), `ColumnPlan::build` unchanged (the tree column needs no sheet).
- **Deliberately not here:** the DuckDB store and the `pricer_sheets` declaration (Part 4), catalogue underlyings (decision 17), sign colouring (decision 15), resizable columns (the vocabulary's known gap).
- **Type consistency checked:** `PricerSettings`, `Shared`, `PricerFactory::{new, reload, view_names, settings}`, `Cursor`, `Entry`, `Editor`, `Menu`/`MenuItem`, `EditorPaint`, `ChoicePaint`, `Loaded`, `MemorySheetStore::{get, save_count, set_refusing, set_pending}`, `GridModel::{build, grid_row_of, entry_row}`, `Paints::{derive, text}` are spelled the same in every task that names them; `SheetDelegate::new` gains its `tile` parameter in Task 10 and Task 10 says to update the one call site.
- **Risk to watch in review:** the three gpui calls with no in-repo precedent — `Context::on_release` (Task 6), `observe_global::<gpui_component::Theme>` (Task 6), and `deferred(..)` for the typeahead (Task 10). Each has a test that exercises the route; if the pinned API differs, the fix is local to that call.
