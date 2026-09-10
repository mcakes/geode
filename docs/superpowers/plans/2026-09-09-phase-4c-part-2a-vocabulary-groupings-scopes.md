# Phase 4c, Part 2a — Reverse Stepping, Consistency, Groupings and Scopes

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Close the two vocabulary gaps that gate every remaining adapter, make the dialog's destructive verbs honour instant-apply like everything else, then add the two thin adapters — Groupings and Scopes — on a settled vocabulary.

**Architecture:** `shell::objectdialog` already hosts `Domain::Views` with instant, memory-first editing (an edit joins a batch; the 250 ms flush merges via `Config::from_docs`, applies through the one applier `hot_reload::apply_reload`, and writes the file). This plan adds `shift+space` to the shared modal vocabulary, routes `d`/`r` through the same batch, and adds two `Domain` variants whose adapters supply only a doc name, a summary and a field list.

**Tech Stack:** Rust, gpui + gpui-component (pinned), `toml_edit`, criterion, `scripts/mutation-check.sh`.

**Spec:** `docs/superpowers/specs/2026-09-08-geode-phase-4c-config-dialogs-design.md` — read §3 (scaffold, field kinds), §7.1 (applying), §8.2 (Groupings), §8.4 (Scopes), and **§16 "As built — Part 1"**.
**Also binding:** `docs/superpowers/specs/2026-09-08-geode-dialog-interaction-model-design.md` — the two-mode key vocabulary.

## Why this plan stops short of Sources

Sources (§8.3) is four-fifths `Text` fields, and `FieldKind::Text` shipped with no key and no editor. Text editing under instant-apply is a design question in its own right — it needs a third momentary mode and a rule about blocking on invalid intermediate states — and Sources additionally restarts the data engine through a swappable `DataHandle`. That is Part 2b, together with the schema inspector, width editing and per-field diagnostics. Groupings and Scopes need none of it, and they prove the vocabulary this plan settles.

## Rulings already made — implement these, do not revisit

1. **"Load into frame" is dropped from the Scopes adapter.** §8.4 specifies it; it is a duplicate. `scope::<name>` is already registered per saved scope (`defaults.rs`, `register_scope_actions`) and `:scope load <name>` already exists in the tile command line. It would also break the action bar's shape three ways at once — it changes frame state rather than config, has nothing to confirm, and closing the dialog is part of its meaning.
2. **`name` is NOT a `Text` field on either adapter.** §8.2 and §8.4 both specify one. Under instant-apply a per-keystroke rename writes a table per prefix and orphans the old one. Rename is out of scope here; say so in the As-built rather than half-building it.
3. **An error-severity diagnostic blocks the batch** (user ruling). Today `Domain::validate`'s diagnostics only paint. An error-severity diagnostic from the edited document must stop the edit joining the batch, because §7.1's no-carry-forward rule means `reload::decide` would otherwise reject the merge — a silent in-memory no-op *while the write still fires*, which is the memory/disk divergence `apply.rs` exists to prevent. Part 2a lands the rule; Part 2b's `Text` is what makes it reachable by typing.
4. **`d`/`r` go through the batch.** They currently call `spawn_removals`, which writes the file and returns with no in-memory merge, so a delete is invisible until the 500 ms watcher. Every other mutation in this dialog is instant; these two should not be the exception.

## Global Constraints

- CI runs on **macOS and Windows**: `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`, `cargo bench --workspace --no-run`, `cargo check -p geode-shell --features test-support --all-targets`.
- **TDD**: failing test, run it, watch it fail for the right reason, then implement.
- **A mutation entry for every behaviour changed.** **Commit before mutating.** Before adding an anchor, confirm it occurs exactly once **as a substring** — the harness does a single `str.replace(…, 1)`. Re-check pre-existing entries in any file you touch; a refactor silently orphans them, and this has happened repeatedly on this work.
- **Run the harness DETACHED**, never in a timed foreground call — a foreground run killed mid-entry once left a mutated file and a stale lock in the tree. Scope with `--changed=main` and say what it selected. Verify `git status --porcelain` is empty afterwards.
- Nothing may stall the render thread; config writes go on the background executor.
- **Never a raw colour** — every colour from `cx.theme()` tokens.
- `geode-shell` never depends on `geode-data`. No crate but `geode-data` opens a file or socket, except `geode-shell` writing its own config under `user_dir`.
- Doc comments explain WHY, densely. **A comment that contradicts the code is a defect** — this work has produced six such claims, every one found by a reviewer looking for them.

## As-built vocabulary this plan builds on

```rust
// crates/geode-shell/src/dialogmode.rs — normal_command's SHIFT branch today
if ks.mods == SHIFT {
    return match ks.key.as_str() {
        "j" => Some(NormalCommand::MoveItem(1)),
        "k" => Some(NormalCommand::MoveItem(-1)),
        "g" => Some(NormalCommand::Nav(NavCommand::Bottom)),
        _ => None,
    };
}

// crates/geode-shell/src/shell/objectdialog/mod.rs
pub enum Confirm { Delete, Revert, Fork }
// FieldKind::Number steps FORWARD ONLY and refuses at max:
FieldKind::Number { value, min, max } => {
    if *value >= *max { return false; }
    *value = (*value + 1).clamp(*min, *max);
    true
}
```

`Domain`'s whole surface is `doc`/`title`/`summary_fn`/`objects`/`fields`/`draft`/`to_table`/`validate`, all taking only `&Config`. `derive_rows` is a private free function with no other caller, so an adapter cannot fork the layer/override walk. Keep both properties.

---

## File Structure

| File | Responsibility |
|---|---|
| `crates/geode-shell/src/dialogmode.rs` | `shift+space` → a new `NormalCommand` variant for reverse stepping. |
| `crates/geode-shell/src/shell/objectdialog/mod.rs` | `Toggle`'s reverse twin on `Choice`/`Number`; the error-diagnostic block; `Domain::{Groupings, Scopes}`; `Confirm::Overwrite`. |
| `crates/geode-shell/src/shell/objectdialog/apply.rs` | `d`/`r` removals join the batch instead of writing directly. |
| `crates/geode-shell/src/shell/objectdialog/render.rs` | The reverse-step key; `Verb('o')`; the blocked-edit notice. |
| `crates/geode-shell/src/shell/objectdialog/groupings.rs` | **NEW.** The `Domain::Groupings` adapter. |
| `crates/geode-shell/src/shell/objectdialog/scopes.rs` | **NEW.** The `Domain::Scopes` adapter. |
| `crates/geode-shell/src/defaults.rs` | `config::groupings`, `config::scopes` — palette-only, no default binding. |
| `crates/geode-shell/src/shell/tests/objectdialog.rs` | Real-key-dispatch coverage for all of it. |
| `scripts/mutation-check.sh`, `CLAUDE.md`, the spec | Entries, count, As-built corrections. |

---

### Task 1: Reverse stepping

**Files:** `dialogmode.rs`, `objectdialog/mod.rs`, `objectdialog/render.rs`, `shell/tests/objectdialog.rs`

**Interfaces produced:** a `NormalCommand` variant meaning "step the selected row's value backward", reached by `shift+space`; `Choice` and `Number` both stepping in both directions.

This gates both adapters below: Groupings' `slot` is a `Number` that can currently be raised and never lowered, and a `Choice` wraps forward only.

- [ ] **Step 1: Write the failing tests**

In `dialogmode.rs`'s `mod tests`, using its existing local `SHIFT` const (`Modifiers::SHIFT` does not exist in this codebase):

```rust
#[test]
fn shift_space_steps_a_value_backward() {
    assert_eq!(normal_command(&ks("space", SHIFT)), Some(NormalCommand::ToggleBack));
    // The forward key is unchanged, and shift+j/k still move items.
    assert_eq!(normal_command(&bare("space")), Some(NormalCommand::Toggle));
    assert_eq!(normal_command(&ks("j", SHIFT)), Some(NormalCommand::MoveItem(1)));
}
```

In `objectdialog/mod.rs`'s `mod tests`:

```rust
/// `Number` refused at `max` and had no way down at all, so the first
/// adapter with a real `Number` — Groupings' `slot` — would inherit a
/// field that can be raised and never lowered.
#[test]
fn a_number_steps_both_ways_and_stops_at_each_end() {
    // A Number field at min: stepping back is a no-op returning false;
    // stepping forward moves it. At max: forward is a no-op, back moves.
    // Assert the returned bool too — it is what marks the draft dirty.
}

/// `Choice` wraps forward; it must wrap backward symmetrically, or the
/// last option is three keystrokes away and the first is unreachable
/// from it.
#[test]
fn a_choice_wraps_in_both_directions() {
    // From index 0, back → last. From last, forward → 0.
}
```

Fill both bodies using the file's existing field-construction helpers — read them first.

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo test -p geode-shell --features test-support dialogmode` and `... objectdialog`
Expected: FAIL to compile — no `ToggleBack` variant.

- [ ] **Step 3: Implement**

Add the variant to `NormalCommand` with a doc comment saying why it exists (the vocabulary had no backward step, so every steppable kind was forward-only). Add `"space" => Some(NormalCommand::ToggleBack)` to `normal_command`'s `SHIFT` branch. Give `Draft` a reverse twin of `toggle_selected` — or a direction parameter, your call, but say which and why in the report; a direction parameter keeps the two paths from drifting, which is the failure this codebase keeps hitting.

Wire it in `render.rs`'s edit-stage match beside `Toggle`, and extend the footer hint.

- [ ] **Step 4: Run the tests, then the full gate**

```bash
cargo test -p geode-shell --features test-support
cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace
```

- [ ] **Step 5: Commit, then the harness (detached)**

Entries for: `shift+space` mapping to nothing (the mutation that restores forward-only), and `Number` refusing to step down. Both are invisible to a green suite today.

---

### Task 2: An error diagnostic blocks the batch

**Files:** `objectdialog/mod.rs`, `objectdialog/render.rs`, `shell/tests/objectdialog.rs`

**Ruling (user):** an error-severity diagnostic from the edited document must stop the edit joining the batch.

Today `commit_or_confirm` checks only `is_dirty()` and `would_fork`; `Domain::validate`'s diagnostics are display-only. §7.1's no-carry-forward rule means an error diagnostic makes `reload::decide` reject the merge — so the edit would be a **silent in-memory no-op while the file write still fires**, diverging memory from disk. That is reachable today only through a pathological `Choice`, and becomes reachable by typing the moment Part 2b lands `Text`. Land the rule now, while it is cheap.

- [ ] **Step 1: Write the failing test**

```rust
/// A draft whose own reader rejects it must not reach the batch: the
/// merge would be refused and the write would still fire, leaving memory
/// and disk disagreeing.
#[gpui::test]
fn an_edit_the_reader_rejects_does_not_join_the_batch(cx: &mut gpui::TestAppContext) {
    // Open a view, drive the draft into a state `Domain::validate` rates
    // Severity::Error, then assert: no pending batch, the notice names the
    // problem, and after the debounce no file was written.
}
```

Construct the invalid state through the dialog's own keys if any path reaches it; if none does, drive `Draft` directly and say so in the report — a test that cannot reach the state through the UI still pins the rule for Part 2b's `Text`, which will reach it.

- [ ] **Step 2: Run it, confirm it fails** (the edit currently joins the batch and a file appears).

- [ ] **Step 3: Implement**

Gate the commit on validation: an error-severity diagnostic refuses, sets a notice naming what the reader objected to, and leaves the draft as the user typed it so they can correct it rather than losing their work. **Warnings must not block** — a desk renaming a column produces warnings by design (§3's "a stale name warns, never errors"), and blocking on those would make a personal file unsaveable.

- [ ] **Step 4: Gate, commit, harness (detached).** Entry: the gate removed, so an invalid edit joins the batch again.

---

### Task 3: `d` and `r` join the batch

**Files:** `objectdialog/apply.rs`, `objectdialog/render.rs`, `shell/tests/objectdialog.rs`

`spawn_removals` writes the file and returns with no in-memory merge, so a delete or revert is invisible until the 500 ms watcher — while every other mutation in this dialog is instant. The dialog's own notice currently hedges with an ellipsis (`"deleting tree in views.toml…"`), which is the tell.

- [ ] **Step 1: Write the failing test** — a `d` on a user-layer object removes it from the browse list **before** the watcher would fire (assert the row is gone after the flush, with no watcher tick), and the file follows.

- [ ] **Step 2: Run it, confirm it fails.**

- [ ] **Step 3: Implement.** Route removals through the same batch and flush as an edit, so one path applies everything. Keep the confirm. The notice loses its ellipsis because the outcome is no longer pending.

- [ ] **Step 4: Gate, commit, harness (detached).** Entry: the removal bypassing the batch again.

---

### Task 4: The Groupings adapter

**Files:** `objectdialog/groupings.rs` (new), `objectdialog/mod.rs`, `defaults.rs`, `shell/tests/objectdialog.rs`

**Spec:** §8.2. Fields: `slot` (`Number`, 1–9), `dimensions` (`OrderedList` over `pickable_columns(config)`, no per-item width). All `Destination::Doc`. **No `name` field** (ruling 2).

- [ ] **Step 1: Read `GroupingSlots::from_doc` in `geode-core`** and the `groupings` doc's real shape before writing anything. The adapter's `to_table` must round-trip through that reader — `Domain::validate` runs it.

- [ ] **Step 2: Write the failing tests** — the adapter's `objects` lists the configured slots with their owning layer; `fields` offers the dataset's pickable columns in the `OrderedList`; `to_table` round-trips through `GroupingSlots::from_doc`; a slot number outside 1–9 is refused by the `Number`'s own bounds rather than by the reader.

- [ ] **Step 3: Run them, confirm they fail.**

- [ ] **Step 4: Implement**, adding the `Domain::Groupings` variant. **`Domain::objects` must stay unconditional** over `doc()` and `summary_fn()` — the per-domain match supplies only those, so an adapter cannot compute the layer/override markers itself. That property was added deliberately; do not reintroduce a per-domain row walk.

Register `config::groupings` in `defaults.rs` with **no default key binding** (palette-only, as every other config dialog is).

- [ ] **Step 5: End-to-end** — `config::groupings` opens, reordering slot 3's dimensions applies instantly, and `ctrl+3` then regroups by the new order. That last assertion is the one that proves the edit reached the frame, not just the file.

- [ ] **Step 6: Gate, commit, harness (detached).**

---

### Task 5: The Scopes adapter

**Files:** `objectdialog/scopes.rs` (new), `objectdialog/mod.rs`, `objectdialog/render.rs`, `defaults.rs`, `shell/tests/objectdialog.rs`

**Spec:** §8.4, as amended by rulings 1 and 2 — **no "Load into frame", no `name` field.** The adapter is the thinnest: its rows are the saved scopes, its edit stage shows a read-only summary of what each selects, and its verbs are `d`, `r` (both free from the scaffold) plus one new one.

- [ ] **Step 1: Read `geode_core::scopes::saved_scopes_from_doc`** and `shell::saved_scopes`, plus `frame::persist_scope_to_user_config`, before writing anything.

- [ ] **Step 2: Write the failing tests**

```rust
/// The one genuinely new verb: `o` overwrites the saved scope under the
/// cursor with whatever the frame currently holds. It writes config and
/// acts on the object under the cursor, so it fits the action bar — only
/// its *input* comes from outside `Config`.
#[gpui::test]
fn o_overwrites_the_saved_scope_with_the_frames_current_one(cx: &mut gpui::TestAppContext) {
    // Set a frame scope, open config::scopes on a saved scope whose
    // values differ, press `o`, answer the confirm, and assert the
    // scopes doc now holds the frame's selection — and that the frame
    // itself is unchanged.
}

/// And it must confirm, because it destroys the saved scope's contents.
#[gpui::test]
fn o_confirms_before_overwriting(cx: &mut gpui::TestAppContext) { }
```

- [ ] **Step 3: Run them, confirm they fail.**

- [ ] **Step 4: Implement.** Add `Confirm::Overwrite` beside `Delete`/`Revert`/`Fork`, and a `Verb('o')` arm in `render.rs`, which already has `shell.frame` in scope. **Do NOT widen `Domain`** to take a `Frame` — that would put gpui entity state in the pure core. Read the frame at the call site and render the new value into the batch through `apply::commit_edit`, not through a direct `config_write` call, so the one applier still applies it.

Register `config::scopes`, palette-only.

- [ ] **Step 5: Gate, commit, harness (detached).** Entries: `o` writing without confirming; `o` mutating the frame instead of the doc.

---

### Task 6: Docs and harness

**Files:** `CLAUDE.md`, the 4c spec, `scripts/mutation-check.sh`

- [ ] **Step 1: `CLAUDE.md`** — record reverse stepping, the error-diagnostic block, that `d`/`r` are instant like everything else, and the two new dialogs with their palette-only actions.

- [ ] **Step 2: Correct the spec.** An investigation found eight stale points; fix the ones this plan touches and record the rest as still-outstanding:
  - §16 claims `Diagnostic.path` does not exist. **It does** — `path` and `with_path` landed with the 4b merge, *after* §16 was written. The remaining blocker for per-field diagnostics is that no reader fills it, which is Part 2b.
  - §8.2 and §8.4 both specify a `name` `Text` field; record ruling 2 and that rename is unbuilt.
  - §8.4's "Load into frame" — record ruling 1 and why.
  - §11 says a failed write "leaves the draft intact so it can be retried"; as built it **reverts** memory and reports to the status bar.
  - §13's mutation-entry list still names "a field edit writing immediately instead of staging into the draft" — an entry against deleted machinery.
  - `Domain::writable()` (§4, §9) does not exist; the schema inspector adds it in Part 2b.
  - §16's `bridge.rs:209` reference is now `bridge.rs:303`.
  - §8.3's reload prompt has no save keystroke to hang on any more — it must trigger from the flush, and it will fire mid-typing unless Part 2b's `Text` commit-on-`enter` lands first. Record this as a Part 2b constraint.

- [ ] **Step 3: The harness count** in `CLAUDE.md`:
```bash
echo $(( $(grep -c '^run_mutation' scripts/mutation-check.sh) - $(grep -c '^run_mutation()' scripts/mutation-check.sh) ))
```

- [ ] **Step 4:** Run `zsh scripts/mutation-check.sh --changed=main` detached; every line must read `caught`, with no `caught*` (an entry caught by a test other than the one it names). Report any.

---

## Self-Review

**Spec coverage.** §8.2 → Task 4. §8.4 → Task 5 (as amended). §3.3's reverse stepping → Task 1. §7.2's validation becoming load-bearing → Task 2. §10's palette-only actions → Tasks 4 and 5. **Deliberately in Part 2b:** §8.3 Sources with its reload prompt and swappable `DataHandle`, §9 the schema inspector, §8.5 per-field diagnostics, width editing, and `FieldKind::Text`/`MultiChoice`.

**Placeholders.** Task 1's two `objectdialog` tests and Tasks 2–5's `#[gpui::test]` bodies are specified by their required assertions rather than as literal code, because the fixtures live in files the implementer must read first; each names exactly what it must assert. Every other step carries real code or a real command.

**Type consistency.** `NormalCommand::ToggleBack` is used in Task 1 as named and nowhere else reshapes it. `Confirm::Overwrite` in Task 5 extends the enum quoted above. `FieldKind::Number`'s bounds are `i64` as built, and `pickable_columns` returns what Task 4's `OrderedList` consumes — check both against the source before writing, since neither was re-verified when this plan was written.
