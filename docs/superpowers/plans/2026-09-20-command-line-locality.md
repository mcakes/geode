# Command-Line Locality Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make every `:` line change only the tile it was typed on, give the blotter a tile-level as-of override, and move the two frame/app-wide commands that had no other door (`:scope <expr>`, `:level`) to the palette.

**Architecture:** Each module's pure parser (`core/commands.rs`) keeps the removed words as `Command::Refused(&'static str)` so a typed one explains where it went; the blotter tile gains `TileAsOf { Follow, Pinned(AsOf) }` beside `Pin` and `tile_scope`, read in `requery` and guarded in `differs_on_followed`. The shell gains one new modal (`shell/scope_expr_view.rs`, in `asof_view`'s mould) and one new `choicedialog::Target` (`LogLevel`, two steps over the existing `ChoiceList`). A sweep test per module runs every vocabulary word and asserts the frame counters, the frame's pending slot persist and the `Diagnostics` entity's pending requests are untouched.

**Tech Stack:** Rust, gpui (pinned `gpui-pre` 0.3.5) + gpui-component 0.6.2, `TestAppContext`/`VisualTestContext` for window tests, `scripts/mutation-check.sh` for the harness.

**Spec:** `docs/superpowers/specs/2026-09-20-geode-command-line-locality-design.md`

## Global Constraints

- Every crate must keep building on macOS and Windows; CI runs `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`, `cargo bench --workspace --no-run`, and `cargo check -p geode-shell --features test-support --all-targets`.
- Run `zsh scripts/mutation-check.sh --anchors-only` before the merge; it must exit 0. Every harness entry names its expected test as the 6th argument.
- Every dialog opens through `dialog::open_shell_dialog_with_key` (never `window.open_dialog`); a mouse-opened dialog's test must TYPE after the click, not only assert the state is `Some`.
- Chip colours go through `chip::chip_paint`; the pinned as-of chip is `Tone::Neutral`.
- `InputState::set_value` emits no `Change` event: anything that writes the shared `dialog_input` must update its own state in the same call.
- No `format!` on a per-render path where a cached `SharedString` will do (the blotter header caches `filter_tip`; the as-of chip caches the same way).
- Palette titles that open a dialog end in `…`; a dialog's own title does not.
- Commit after every task with the attribution trailer `Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>`.
- Work on a worktree branch (`superpowers:using-git-worktrees`), never on `main` directly.

---

### Task 1: Blotter parser — refusals, `:asof live|clear`, removals

**Files:**
- Modify: `crates/geode-blotter/src/core/commands.rs` (whole file: enum lines 7–31, `COMMANDS` line 33, `parse` lines 45–168, `Vocabulary` lines 170–187, `completions` lines 191–262, tests 264–528)

**Interfaces:**
- Produces: `pub enum AsOfArg { At(String), Live, Clear }`; `Command::AsOf(AsOfArg)`; `Command::Refused(&'static str)`; `pub const COMMANDS: [&str; 7]`; `pub const REFUSED_SCOPE`, `REFUSED_ASOF_UNDO`, `REFUSED_LIVE`, `REFUSED_GROUP_SAVE: &str`. `Vocabulary` loses its `scopes` field. Deleted variants: `GroupSave`, `ScopeExpr`, `ScopeText`, `ScopeClear`, `ScopeUndo`, `ScopeRedo`, `ScopeDrop`, `ScopeSave`, `ScopeLoad`, `AsOfUndo`, `Live`.
- Task 2 wires the tile's `command` to the new enum. **This task leaves `crates/geode-blotter/src/tile.rs` not compiling until Task 2 lands; run only the parser's own tests here** (`cargo test -p geode-blotter --lib core::commands` will not build either, since the crate is one unit — so in this task write the tests and the implementation, confirm with `cargo check -p geode-blotter 2>&1 | grep commands.rs` that the parser file itself has no errors, and run the tests at the end of Task 2). Commit the two tasks separately anyway: Task 1's commit is the parser, Task 2's the tile.

- [ ] **Step 1: Replace the enum, the command list and the refusal constants**

Replace lines 7–35 (`#[derive…] pub enum Command { … }` through `const COMMANDS`) with:

```rust
/// A `:asof` argument (command-line locality spec §3.2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AsOfArg {
    /// Pin the tile to this instant; parsed by `parse_as_of` at apply time.
    At(String),
    /// Pin the tile to live.
    Live,
    /// Follow the frame again.
    Clear,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Group(Vec<String>),
    GroupSlot(u8),
    Unpin,
    Unscoped,
    FilterExpr(String),
    FilterText(String),
    FilterClear,
    AsOf(AsOfArg),
    View(String),
    Sort { column: String, order: SortOrder },
    SortClear,
    /// A word this line no longer runs because it was frame-wide
    /// (command-line locality spec §5): the message names the door it
    /// moved to. The tile shows it inline like any parse error. Never a
    /// completion — a refusal is not a suggestion.
    Refused(&'static str),
}

/// The words `completions` offers at the start of a line — every word
/// `parse` accepts EXCEPT the refusals. The tile's sweep test
/// (`every_colon_command_leaves_the_frame_alone`) reads this list so a
/// word added here is swept the day it lands.
pub const COMMANDS: [&str; 7] = ["asof", "filter", "group", "sort", "unpin", "unscoped", "view"];

/// The refusal messages (spec §5). Frame-wide verbs left the `:` line on
/// 2026-09-20; each message names the door that replaced it.
pub const REFUSED_SCOPE: &str =
    ":scope is frame-wide — the scope bar (mod+/), Set scope expression…, or the palette's Scope: entries";
pub const REFUSED_ASOF_UNDO: &str =
    "frame as-of undo is in the palette (Swap to the previous as of)";
pub const REFUSED_LIVE: &str =
    ":asof live pins this tile; Return to live (palette) sets the frame";
pub const REFUSED_GROUP_SAVE: &str =
    "saving a slot is in the Groupings dialog (palette: Edit groupings…)";
```

- [ ] **Step 2: Rewrite the `parse` arms**

In `parse`, replace the `"live"`, `"group"`, `"scope"` and `"asof"` arms:

```rust
        "unpin" => Ok(Command::Unpin),
        "unscoped" => Ok(Command::Unscoped),
        "live" => Ok(Command::Refused(REFUSED_LIVE)),
        "group" => {
            let mut words = rest.split_whitespace();
            match words.next() {
                None => Err("group needs columns or `slot N`".into()),
                Some("slot") => slot(words.next(), "group slot").map(Command::GroupSlot),
                Some("save") => Ok(Command::Refused(REFUSED_GROUP_SAVE)),
                Some(_) => {
                    let columns: Vec<String> = rest
                        .split(|c: char| c == ',' || c.is_whitespace())
                        .filter(|s| !s.is_empty())
                        .map(str::to_string)
                        .collect();
                    Ok(Command::Group(columns))
                }
            }
        }
        "scope" => Ok(Command::Refused(REFUSED_SCOPE)),
```

and

```rust
        "asof" => match rest {
            "" => Err(
                "asof needs a time (HH:MM, HH:MM:SS, YYYY-MM-DD[ HH:MM[:SS]] or RFC 3339), \
                 `live` or `clear`"
                    .into(),
            ),
            "undo" => Ok(Command::Refused(REFUSED_ASOF_UNDO)),
            "live" => Ok(Command::AsOf(AsOfArg::Live)),
            "clear" => Ok(Command::AsOf(AsOfArg::Clear)),
            t => Ok(Command::AsOf(AsOfArg::At(t.to_string()))),
        },
```

Leave `filter`, `view`, `sort` and the `other =>` arm as they are.

- [ ] **Step 3: Drop `Vocabulary.scopes` and the removed completion arms**

Replace the `Vocabulary` struct with:

```rust
#[derive(Debug, Clone, Default)]
pub struct Vocabulary {
    /// What `sort` can rank: the view's own column plan (the tree column
    /// plus its declared measures) — what is actually displayed, not the
    /// dataset's full dimension set.
    pub columns: Vec<String>,
    /// Every column the tile's dataset carries as a dimension at any
    /// grain it has, plus every derived dimension (Phase 4a §3.2, §6.8)
    /// — distinct from `columns` since a dimension the view does not
    /// display (e.g. `book`, `currency`) is still a legal `group` or
    /// `filter` target. `group` completes from this alone; `filter`
    /// completes from this union `columns` (an expression can also name
    /// a measure).
    pub dimensions: Vec<String>,
    pub views: Vec<String>,
}
```

In `completions`, replace the `["group"]` arm, delete the `["group", "slot"] | ["group", "save"]` arm's `save` half, delete every `["scope" …]` arm, and replace the `["asof"]` arm:

```rust
        ["group"] => {
            let mut v = vocab.dimensions.clone();
            v.push("slot".into());
            v
        }
        ["group", "slot"] => (1..=9).map(|n| n.to_string()).collect(),
        ["group", ..] => vocab.dimensions.clone(),
```

```rust
        ["asof"] => vec!["clear".into(), "live".into()],
```

- [ ] **Step 4: Rewrite the parser tests**

In `mod tests`: `vocab()` loses `scopes: vec![]`. In `every_command_parses`, delete the `group save 9`, `scope …` (all four), and `live` assertions and add:

```rust
        assert_eq!(
            parse("asof 14:05").unwrap(),
            Command::AsOf(AsOfArg::At("14:05".into()))
        );
        assert_eq!(parse("asof live").unwrap(), Command::AsOf(AsOfArg::Live));
        assert_eq!(parse("asof clear").unwrap(), Command::AsOf(AsOfArg::Clear));
```

Delete the test `new_scope_and_asof_forms_parse` and `completions_offer_dimensions_after_drop_and_scope_names_after_load`. In `completions_follow_the_argument_position` and `group_and_drop_complete_dimensions_not_measures`, delete every assertion on a `scope …` line and change any `["asof"]` expectation to `["clear", "live"]` and any `group` first-word expectation to drop `"save"`. Then add:

```rust
    /// Command-line locality (2026-09-20): the frame-wide words are
    /// refusals whose message names the door, and none is a completion.
    #[test]
    fn frame_wide_words_are_refusals_that_name_their_door() {
        assert_eq!(parse("scope lhu = 'L1'").unwrap(), Command::Refused(REFUSED_SCOPE));
        assert_eq!(parse("scope clear").unwrap(), Command::Refused(REFUSED_SCOPE));
        assert_eq!(parse("asof undo").unwrap(), Command::Refused(REFUSED_ASOF_UNDO));
        assert_eq!(parse("live").unwrap(), Command::Refused(REFUSED_LIVE));
        assert_eq!(parse("group save 3").unwrap(), Command::Refused(REFUSED_GROUP_SAVE));
        for refused in ["scope", "live"] {
            assert!(!COMMANDS.contains(&refused), "`{refused}` must not be offered");
        }
        let first = completions("", 0, &vocab());
        assert_eq!(first, COMMANDS.iter().map(|s| s.to_string()).collect::<Vec<_>>());
        assert_eq!(completions("asof ", 5, &vocab()), vec!["clear", "live"]);
        assert_eq!(
            completions("group ", 6, &vocab()),
            vec!["book", "lhu", "slot"],
            "`save` is no longer offered after `group`"
        );
    }
```

- [ ] **Step 5: Check the parser file compiles on its own terms**

Run: `cargo check -p geode-blotter 2>&1 | grep -c "core/commands.rs"`
Expected: `0` (every remaining error names `tile.rs`, fixed in Task 2).

- [ ] **Step 6: Commit**

```bash
git add crates/geode-blotter/src/core/commands.rs
git commit -m "blotter: frame-wide : words become refusals; :asof live|clear parse (locality §5, §3.2)

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

### Task 2: Blotter tile — `TileAsOf`, the command arms, the request and the followed guard

**Files:**
- Modify: `crates/geode-blotter/src/tile.rs` — enum `Pin` (line 89), struct fields (lines 119–125), `new` restore block (lines 215–262), `differs_on_followed` (lines 431–437), `requery` (lines 551–560), `command` (lines 926–1114), `completions` (line 1170, the `scopes` line), the test around line 2395 that runs `scope lhu = 'L1'`.

**Interfaces:**
- Consumes: Task 1's `AsOfArg`, `Command::Refused`, `REFUSED_*`.
- Produces: `pub enum TileAsOf { Follow, Pinned(AsOf) }`; field `tile_as_of: TileAsOf`; `fn set_tile_as_of(&mut self, next: TileAsOf) -> bool` (true when changed; Task 3 extends it to refresh the chip cache). Tests in later tasks read `t.tile_as_of`.

- [ ] **Step 1: Write the failing tile tests**

Add to `mod tests` in `tile.rs`, after `an_unscoped_tile_still_applies_its_own_filter`:

```rust
    /// Command-line locality spec §3: `:asof <time>` pins THIS tile, the
    /// frame's own as-of untouched; `:asof live` pins it to live under a
    /// historical frame; `:asof clear` follows again.
    #[gpui::test]
    fn asof_pins_the_tile_and_leaves_the_frame_alone(cx: &mut gpui::TestAppContext) {
        let (h, mut cx) = open(cx);
        h.tile.update(&mut cx, |t, cx| t.set_visible(true, cx));
        let _ = next_query(&h.requests);
        let frame_as_of_version = h.frame.read_with(&cx, |f, _| f.versions().as_of);

        h.tile
            .update(&mut cx, |t, cx| t.command("asof 14:05", cx).unwrap());
        let p = next_query(&h.requests);
        assert!(matches!(p.as_of, AsOf::At(_)), "the request carries the pin");
        assert!(
            h.frame.read_with(&cx, |f, _| f.as_of().is_live()),
            "the frame is still live"
        );
        assert_eq!(
            h.frame.read_with(&cx, |f, _| f.versions().as_of),
            frame_as_of_version,
            "the frame's as-of counter did not move"
        );
        assert!(matches!(
            h.tile.read_with(&cx, |t, _| t.tile_as_of.clone()),
            TileAsOf::Pinned(AsOf::At(_))
        ));

        // Pinning the same value again is a no-op: no requery.
        h.tile
            .update(&mut cx, |t, cx| t.command("asof clear", cx).unwrap());
        let p = next_query(&h.requests);
        assert!(p.as_of.is_live(), "following again queries at the frame's (live) as-of");
        h.tile
            .update(&mut cx, |t, cx| t.command("asof clear", cx).unwrap());
        assert!(
            h.requests.recv_timeout(Duration::from_millis(200)).is_err(),
            "clearing an already-following tile requeries nothing"
        );

        // Live under a historical frame.
        h.frame.update(&mut cx, |f, cx| {
            f.set_as_of(AsOf::At(chrono::Utc::now()));
            cx.notify();
        });
        let p = next_query(&h.requests);
        assert!(matches!(p.as_of, AsOf::At(_)), "a following tile follows");
        h.tile
            .update(&mut cx, |t, cx| t.command("asof live", cx).unwrap());
        let p = next_query(&h.requests);
        assert!(p.as_of.is_live(), "pinned to live under a historical frame");
        assert!(
            matches!(h.frame.read_with(&cx, |f, _| f.as_of().clone()), AsOf::At(_)),
            "the frame stayed historical"
        );
    }

    /// A pinned tile does not follow the frame's as-of (spec §3.3): a
    /// frame change neither requeries it nor holds the barrier for it —
    /// the tile self-arrives through `Frame::arrived`, as a pinned-
    /// grouping tile does.
    #[gpui::test]
    fn a_pinned_tile_ignores_the_frames_as_of_and_answers_the_barrier(
        cx: &mut gpui::TestAppContext,
    ) {
        let (h, mut cx) = open(cx);
        h.tile.update(&mut cx, |t, cx| t.set_visible(true, cx));
        let _ = next_query(&h.requests);
        h.tile
            .update(&mut cx, |t, cx| t.command("asof 14:05", cx).unwrap());
        let p = next_query(&h.requests);
        deliver(&h, &mut cx, p.tag, Ok(snapshot()));

        h.frame.update(&mut cx, |f, cx| {
            f.set_as_of(AsOf::At(chrono::Utc::now()));
            f.open_flip([QueryKey(7)], std::time::Instant::now());
            cx.notify();
        });
        assert!(
            h.requests.recv_timeout(Duration::from_millis(200)).is_err(),
            "a pinned tile does not requery on a frame as-of change"
        );
        assert!(
            !h.frame.read_with(&cx, |f, _| f.barrier_open()),
            "and it answered the barrier without a query"
        );
    }

    /// The refusals (spec §5) reach the trader as the parser's message,
    /// and touch nothing.
    #[gpui::test]
    fn refused_words_error_inline_with_the_doors_name(cx: &mut gpui::TestAppContext) {
        let (h, mut cx) = open(cx);
        h.tile.update(&mut cx, |t, cx| t.set_visible(true, cx));
        let _ = next_query(&h.requests);
        for (line, expected) in [
            ("scope lhu = 'L1'", crate::core::commands::REFUSED_SCOPE),
            ("asof undo", crate::core::commands::REFUSED_ASOF_UNDO),
            ("live", crate::core::commands::REFUSED_LIVE),
            ("group save 2", crate::core::commands::REFUSED_GROUP_SAVE),
        ] {
            let err = h.tile.update(&mut cx, |t, cx| t.command(line, cx)).unwrap_err();
            assert_eq!(err, expected, "`:{line}`");
        }
        assert!(h.requests.try_recv().is_err(), "a refusal requeries nothing");
    }
```

Add `use crate::tile::TileAsOf;` if the tests module does not already `use super::*` (it does — check; `super::*` covers it).

In the existing test around line 2395 (the one that runs `t.command("scope lhu = 'L1'", cx)`), replace the block from that `h.tile.update(... "scope lhu = 'L1'" ...)` line through the `t.command("scope undo", cx).unwrap());` + `let _ = next_query(&h.requests);` pair with:

```rust
        h.tile
            .update(&mut cx, |t, cx| t.command("asof 14:05", cx).unwrap());
        let p = next_query(&h.requests);
        assert!(matches!(p.as_of, geode_core::query::AsOf::At(_)));
        h.tile
            .update(&mut cx, |t, cx| t.command("asof clear", cx).unwrap());
        let p = next_query(&h.requests);
        assert!(p.as_of.is_live());
```

- [ ] **Step 2: Run to verify they fail to compile**

Run: `cargo test -p geode-blotter 2>&1 | grep -E "^error" | head`
Expected: errors naming `Command::GroupSave`/`ScopeExpr`/… in `tile.rs` and `TileAsOf` not found.

- [ ] **Step 3: Add the state**

After `pub enum Pin { … }` (line 93) add:

```rust
/// The tile's as-of (command-line locality spec §3): the third override
/// beside [`Pin`] (grouping) and `tile_scope`/`unscoped` (scope). A
/// pinned tile queries at its own instant and does not follow the
/// frame's `as_of` counter; `:asof clear` returns it to `Follow`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TileAsOf {
    /// Query at the frame's as-of; requery when it changes.
    Follow,
    /// Query at this instant regardless of the frame.
    Pinned(AsOf),
}
```

In the struct, after `unscoped_tip_selector: SharedString,` add:

```rust
    /// The as-of override (spec §3.1); `Follow` on a fresh tile. Tests
    /// read it directly, the way they read `pin`.
    pub(crate) tile_as_of: TileAsOf,
```

In `new`, after the `let tile_scope = …;` block and before `let unscoped_tip_selector`, add `let tile_as_of = TileAsOf::Follow;` (Task 3 replaces this with the session restore), and add `tile_as_of,` to the struct literal wherever `unscoped,` is listed.

- [ ] **Step 4: The request and the followed guard**

In `requery`, replace `(grouping, scope, frame.as_of().clone(), frame.versions())` with:

```rust
            let as_of = match &self.tile_as_of {
                TileAsOf::Follow => frame.as_of().clone(),
                TileAsOf::Pinned(pinned) => pinned.clone(),
            };
            (grouping, scope, as_of, frame.versions())
```

In `differs_on_followed`, replace `|| versions.as_of != now.as_of` with:

```rust
            || (matches!(self.tile_as_of, TileAsOf::Follow) && versions.as_of != now.as_of)
```

and extend its doc comment's first sentence: "`scope` unless it is unscoped, `grouping` unless it is pinned, `as_of` unless the tile's own as-of is pinned (spec §3.3), and always `data`/`config`."

- [ ] **Step 5: The command arms**

Add the setter next to `set_tile_scope`:

```rust
    /// Change the as-of override; `true` when it changed. The one door,
    /// so Task 3's chip cache cannot go stale.
    fn set_tile_as_of(&mut self, next: TileAsOf) -> bool {
        if self.tile_as_of == next {
            return false;
        }
        self.tile_as_of = next;
        true
    }
```

In `command`, delete the arms `Command::GroupSave`, every `Command::Scope*`, `Command::AsOf(text)`, `Command::AsOfUndo`, `Command::Live`, and add:

```rust
            Command::Refused(message) => return Err(message.to_string()),
            Command::AsOf(arg) => {
                let next = match arg {
                    AsOfArg::At(text) => {
                        TileAsOf::Pinned(AsOf::At(parse_as_of(&text, chrono::Utc::now())?))
                    }
                    AsOfArg::Live => TileAsOf::Pinned(AsOf::Live),
                    AsOfArg::Clear => TileAsOf::Follow,
                };
                if self.set_tile_as_of(next) {
                    self.requery(cx);
                }
            }
```

Update the `use crate::core::commands::{…}` line to `{AsOfArg, Command, Vocabulary, completions, parse, parse_as_of}`.

In `completions` (around line 1170) delete `let scopes = self.frame.read(cx).saved_scopes().keys().cloned().collect();` and the `scopes,` field in the `Vocabulary { … }` literal.

- [ ] **Step 6: Run the blotter tests**

Run: `cargo test -p geode-blotter 2>&1 | tail -5`
Expected: all pass, including the three new tests and the parser tests from Task 1.

- [ ] **Step 7: Commit**

```bash
git add crates/geode-blotter/src/tile.rs
git commit -m "blotter: TileAsOf override — :asof pins the tile, never the frame (locality §3.1–3.3)

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

### Task 3: Blotter header chip and session round-trip

**Files:**
- Modify: `crates/geode-blotter/src/tile.rs` — struct fields, `new` (restore + selectors), `set_tile_as_of`, the header render (after the `filtered` chip, ~line 1420, and the provenance `as_of_request` chip ~line 1436), `serialize` (~line 1245).

**Interfaces:**
- Consumes: Task 2's `TileAsOf`, `set_tile_as_of`.
- Produces: debug selectors `blotter-asof-{id}` (the pinned chip), `blotter-asof-frame-{id}` (the provenance chip, following only), tooltip selector `tip-blotter-asof-{id}`; record key `as_of` (`"live"` or RFC 3339); `pub(crate) fn pinned_chip_text(at: DateTime<Utc>, now: DateTime<Utc>) -> String`; `fn as_of_record(&self) -> Option<toml::Value>`.

- [ ] **Step 1: Write the failing tests**

Pure, in `mod tests`:

```rust
    /// The pinned chip reads `AS OF HH:MM` for today in the trader's
    /// local clock and carries the date otherwise (spec §3.4).
    #[test]
    fn pinned_chip_text_elides_todays_date() {
        let now = chrono::Utc::now();
        let today = pinned_chip_text(now, now);
        assert!(today.starts_with("AS OF "), "{today}");
        assert_eq!(today.len(), "AS OF HH:MM".len(), "{today}");
        let old = now - chrono::Duration::days(3);
        let past = pinned_chip_text(old, now);
        assert_eq!(past.len(), "AS OF YYYY-MM-DD HH:MM".len(), "{past}");
    }
```

Window tests (same module, after the Task 2 tests):

```rust
    /// The pinned chip paints from the tile's own state the moment the
    /// line runs, in place of the provenance-driven warning chip; a
    /// following tile under a historical frame paints only the latter.
    #[gpui::test]
    fn a_pinned_tile_paints_the_neutral_chip_and_hides_the_frame_one(
        cx: &mut gpui::TestAppContext,
    ) {
        let (h, mut cx) = open(cx);
        h.tile.update(&mut cx, |t, cx| t.set_visible(true, cx));
        let p = next_query(&h.requests);
        deliver(&h, &mut cx, p.tag, Ok(snapshot()));
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        assert!(cx.debug_bounds("blotter-asof-7").is_none(), "following, live: no chip");

        h.tile
            .update(&mut cx, |t, cx| t.command("asof live", cx).unwrap());
        let p = next_query(&h.requests);
        deliver(&h, &mut cx, p.tag, Ok(snapshot()));
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        assert!(cx.debug_bounds("blotter-asof-7").is_some(), "pinned: the chip paints");
        assert!(
            cx.debug_bounds("blotter-asof-frame-7").is_none(),
            "the provenance chip is suppressed while pinned"
        );

        h.tile
            .update(&mut cx, |t, cx| t.command("asof clear", cx).unwrap());
        let _ = next_query(&h.requests);
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        assert!(cx.debug_bounds("blotter-asof-7").is_none(), "cleared: no chip");
    }

    /// Session (spec §3.5): `as_of` is written only while pinned, as
    /// `"live"` or RFC 3339, restored to the same pin, and a malformed
    /// value restores to `Follow`.
    #[gpui::test]
    fn as_of_round_trips_through_the_session_record(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        let state = h.tile.read_with(&vcx, |t, _| t.serialize());
        assert!(state.get("as_of").is_none(), "following writes nothing");

        h.tile
            .update(&mut vcx, |t, cx| t.command("asof live", cx).unwrap());
        let state = h.tile.read_with(&vcx, |t, _| t.serialize());
        assert_eq!(state["as_of"].as_str(), Some("live"));

        h.tile
            .update(&mut vcx, |t, cx| t.command("asof 2026-09-20 14:05", cx).unwrap());
        let state = h.tile.read_with(&vcx, |t, _| t.serialize());
        let written = state["as_of"].as_str().unwrap().to_string();
        assert!(chrono::DateTime::parse_from_rfc3339(&written).is_ok(), "{written}");

        let mut record = toml::Table::new();
        record.insert("as_of".into(), toml::Value::String(written.clone()));
        let (h2, mut vcx2) = open_with(cx, Some(&record));
        assert!(matches!(
            h2.tile.read_with(&vcx2, |t, _| t.tile_as_of.clone()),
            TileAsOf::Pinned(AsOf::At(_))
        ));
        h2.tile.update(&mut vcx2, |t, cx| t.set_visible(true, cx));
        let p = next_query(&h2.requests);
        assert!(matches!(p.as_of, AsOf::At(_)), "the first request carries the restored pin");

        let mut record = toml::Table::new();
        record.insert("as_of".into(), toml::Value::String("live".into()));
        let (h3, vcx3) = open_with(cx, Some(&record));
        assert_eq!(
            h3.tile.read_with(&vcx3, |t, _| t.tile_as_of.clone()),
            TileAsOf::Pinned(AsOf::Live)
        );

        let mut record = toml::Table::new();
        record.insert("as_of".into(), toml::Value::String("yesterday-ish".into()));
        let (h4, vcx4) = open_with(cx, Some(&record));
        assert_eq!(
            h4.tile.read_with(&vcx4, |t, _| t.tile_as_of.clone()),
            TileAsOf::Follow,
            "a malformed value follows the frame"
        );
    }
```

(`open_with` takes `Option<&toml::Table>`; each call opens its own window, which is what the other session tests do.)

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p geode-blotter pinned_chip_text 2>&1 | grep -E "error|FAILED" | head -3`
Expected: `pinned_chip_text` not found.

- [ ] **Step 3: The chip cache and the pure text helper**

Add the free function near `short_time` (~line 1289):

```rust
/// The pinned chip's text (spec §3.4): `AS OF HH:MM` when `at` falls on
/// today's LOCAL date, `AS OF YYYY-MM-DD HH:MM` otherwise — the same rule
/// the toolbar's readout uses. Local because every displayed time is the
/// trader's clock (Phase 4a ruling).
pub(crate) fn pinned_chip_text(
    at: chrono::DateTime<chrono::Utc>,
    now: chrono::DateTime<chrono::Utc>,
) -> String {
    let local = at.with_timezone(&chrono::Local);
    if local.date_naive() == now.with_timezone(&chrono::Local).date_naive() {
        format!("AS OF {}", local.format("%H:%M"))
    } else {
        format!("AS OF {}", local.format("%Y-%m-%d %H:%M"))
    }
}
```

Add fields after `tile_as_of`:

```rust
    /// The pinned chip's text and tooltip title, cached when
    /// `tile_as_of` changes (`set_tile_as_of`) so `render` clones two
    /// `SharedString`s rather than formatting — the `filter_tip` rule.
    /// Both empty while `Follow`.
    asof_chip: SharedString,
    asof_tip: SharedString,
    /// `"tip-blotter-asof-{id}"`, built once.
    asof_tip_selector: SharedString,
```

Add a helper and make `set_tile_as_of` call it:

```rust
    /// The chip text and tooltip title for `as_of` (spec §3.4).
    fn asof_chip_strings(as_of: &TileAsOf) -> (SharedString, SharedString) {
        match as_of {
            TileAsOf::Follow => (SharedString::default(), SharedString::default()),
            TileAsOf::Pinned(AsOf::Live) => ("LIVE".into(), "Pinned to live".into()),
            TileAsOf::Pinned(AsOf::At(at)) => (
                pinned_chip_text(*at, chrono::Utc::now()).into(),
                format!(
                    "Pinned to {}",
                    at.with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M:%S %Z")
                )
                .into(),
            ),
        }
    }

    fn set_tile_as_of(&mut self, next: TileAsOf) -> bool {
        if self.tile_as_of == next {
            return false;
        }
        let (chip, tip) = Self::asof_chip_strings(&next);
        self.asof_chip = chip;
        self.asof_tip = tip;
        self.tile_as_of = next;
        true
    }
```

In `new`: replace `let tile_as_of = TileAsOf::Follow;` with the restore:

```rust
        // `as_of` (command-line locality spec §3.5): `"live"`, an RFC 3339
        // instant, or absent (following). A value that is neither follows
        // the frame, logged like a restored `filter.expr` that no longer
        // parses.
        let tile_as_of = match restored.and_then(|t| t.get("as_of")).and_then(|v| v.as_str()) {
            None => TileAsOf::Follow,
            Some("live") => TileAsOf::Pinned(AsOf::Live),
            Some(text) => match chrono::DateTime::parse_from_rfc3339(text) {
                Ok(at) => TileAsOf::Pinned(AsOf::At(at.with_timezone(&chrono::Utc))),
                Err(e) => {
                    tracing::warn!(
                        target: "geode::session",
                        "tile {}: restored as_of '{text}' is not RFC 3339 ({e}) — following the frame",
                        tile.0
                    );
                    TileAsOf::Follow
                }
            },
        };
        let (asof_chip, asof_tip) = Self::asof_chip_strings(&tile_as_of);
        let asof_tip_selector: SharedString = format!("tip-blotter-asof-{}", tile.0).into();
```

and add `asof_chip, asof_tip, asof_tip_selector,` to the struct literal.

- [ ] **Step 4: Paint the chip and suppress the provenance one while pinned**

In `render`'s header, directly after the `if !self.tile_scope.is_empty() { … filtered … }` block:

```rust
        // The as-of override's chip (spec §3.4): neutral, like `pinned`
        // and `filtered` — a state the trader chose. Painted from the
        // tile's own state, so it is right from the keystroke, not from
        // the next delivery.
        if let TileAsOf::Pinned(_) = &self.tile_as_of {
            header = header.child(
                div()
                    .id(ElementId::NamedInteger(
                        SharedString::new_static("blotter-asof"),
                        self.tile.0,
                    ))
                    .text_color(neutral_chip.text)
                    .when_some(neutral_chip.fill, |el, fill| el.bg(fill))
                    .px_1()
                    .rounded(theme.radius_tokens().sm)
                    .debug_selector(|| format!("blotter-asof-{}", self.tile.0))
                    .child(self.asof_chip.clone())
                    .tooltip(tips::tip_with(
                        self.asof_tip_selector.clone(),
                        self.asof_tip.clone(),
                        None,
                        Some(SharedString::new_static(":asof clear follows the frame")),
                    )),
            );
        }
```

Change the provenance chip: `if let Some(req) = &p.as_of_request {` becomes

```rust
            // The frame's historical warning (inherited danger) — only
            // while FOLLOWING; a pinned tile's request always carries its
            // pin and the neutral chip above already says so.
            if matches!(self.tile_as_of, TileAsOf::Follow)
                && let Some(req) = &p.as_of_request
            {
```

and add `.debug_selector(|| format!("blotter-asof-frame-{}", self.tile.0))` to that `div()` chain.

- [ ] **Step 5: Serialize**

Add next to `serialize`:

```rust
    /// The session record's `as_of` value (spec §3.5): `None` while
    /// following, `"live"`, or the pinned instant in RFC 3339 (UTC).
    fn as_of_record(&self) -> Option<toml::Value> {
        match &self.tile_as_of {
            TileAsOf::Follow => None,
            TileAsOf::Pinned(AsOf::Live) => Some(toml::Value::String("live".into())),
            TileAsOf::Pinned(AsOf::At(at)) => Some(toml::Value::String(
                at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            )),
        }
    }
```

and in `serialize`, after the `unscoped` insert: `if let Some(v) = self.as_of_record() { t.insert("as_of".into(), v); }`.

- [ ] **Step 6: Run the blotter tests**

Run: `cargo test -p geode-blotter 2>&1 | tail -5`
Expected: all pass.

- [ ] **Step 7: Commit**

```bash
git add crates/geode-blotter/src/tile.rs
git commit -m "blotter: neutral AS OF/LIVE chip and session as_of for the pinned tile (locality §3.4, §3.5)

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

### Task 4: Blotter sweep test and harness entries

**Files:**
- Modify: `crates/geode-blotter/src/tile.rs` (tests), `scripts/mutation-check.sh` (append after the last `commands:` entry near line 2905).

**Interfaces:**
- Consumes: `commands::COMMANDS` (pub, Task 1), `Frame::take_pending_persist`, `Frame::versions/slots/scope/as_of`.

- [ ] **Step 1: Write the sweep test**

```rust
    /// The rule (command-line locality spec §2): a `:` line changes only
    /// this tile. Every word the parser accepts — with a valid argument
    /// where one is needed — plus every refusal, runs against a tile
    /// while the frame's scope/grouping/as-of counters, its slot set and
    /// its pending slot persist are watched. `COMMANDS` is read so a word
    /// added there without a line here fails.
    #[gpui::test]
    fn every_colon_command_leaves_the_frame_alone(cx: &mut gpui::TestAppContext) {
        let (h, mut cx) = open(cx);
        h.tile.update(&mut cx, |t, cx| t.set_visible(true, cx));
        let _ = next_query(&h.requests);
        let lines = [
            "group lhu",
            "group slot 1",
            "unpin",
            "unscoped",
            "unscoped",
            "filter lhu = 'L1'",
            "filter text spx",
            "filter clear",
            "asof 14:05",
            "asof live",
            "asof clear",
            "view wide",
            "sort delta01 desc",
            "sort clear",
            // The refusals.
            "scope lhu = 'L1'",
            "scope clear",
            "scope undo",
            "asof undo",
            "live",
            "group save 1",
        ];
        for word in crate::core::commands::COMMANDS {
            assert!(
                lines.iter().any(|l| l.split_whitespace().next() == Some(word)),
                "no sweep line for `:{word}`"
            );
        }
        let read = |cx: &gpui::VisualTestContext| {
            h.frame.read_with(cx, |f, _| {
                (
                    f.versions().scope,
                    f.versions().grouping,
                    f.versions().as_of,
                    f.slots().clone(),
                    f.scope().clone(),
                    f.as_of().clone(),
                )
            })
        };
        let before = read(&cx);
        for line in lines {
            let _ = h.tile.update(&mut cx, |t, cx| t.command(line, cx));
            while h.requests.try_recv().is_ok() {}
            assert_eq!(read(&cx), before, "`:{line}` reached the frame");
            assert!(
                h.frame
                    .update(&mut cx, |f, _| f.take_pending_persist())
                    .is_none(),
                "`:{line}` queued a slot write"
            );
        }
    }
```

- [ ] **Step 2: Run it**

Run: `cargo test -p geode-blotter every_colon_command_leaves_the_frame_alone 2>&1 | tail -3`
Expected: PASS.

- [ ] **Step 3: Prove it bites (temporary)**

In `command`, change `Command::Refused(message) => return Err(message.to_string()),` to

```rust
            Command::Refused(_) => {
                self.frame.update(cx, |f, cx| {
                    if f.set_as_of(AsOf::At(chrono::Utc::now())) {
                        cx.notify();
                    }
                });
            }
```

Run the sweep test: expected FAIL with "`:scope lhu = 'L1'` reached the frame". Revert the change (`git checkout crates/geode-blotter/src/tile.rs` is NOT safe here — the file holds uncommitted test code; undo the edit by hand).

- [ ] **Step 4: Harness entries**

Append after the `"commands: a bare sort abs is abs desc"` entry in `scripts/mutation-check.sh`:

```zsh
# Command-line locality (2026-09-20) ---------------------------------------

# A refusal must stay a refusal: mutated back into a frame write, the
# sweep sees the frame's as-of move.
run_mutation "locality: a refused word never writes the frame" \
  crates/geode-blotter/src/tile.rs \
  '            Command::Refused(message) => return Err(message.to_string()),' \
  '            Command::Refused(_) => { self.frame.update(cx, |f, cx| { if f.set_as_of(AsOf::At(chrono::Utc::now())) { cx.notify(); } }); }' \
  geode-blotter \
  every_colon_command_leaves_the_frame_alone

# A refused word is never offered as a completion.
run_mutation "locality: refusals are not completions" \
  crates/geode-blotter/src/core/commands.rs \
  '        ["asof"] => vec!["clear".into(), "live".into()],' \
  '        ["asof"] => vec!["clear".into(), "live".into(), "undo".into()],' \
  geode-blotter \
  frame_wide_words_are_refusals_that_name_their_door

# The request carries the pin, not the frame's as-of.
run_mutation "asof-pin: the request carries the pin" \
  crates/geode-blotter/src/tile.rs \
  '                TileAsOf::Pinned(pinned) => pinned.clone(),' \
  '                TileAsOf::Pinned(_) => frame.as_of().clone(),' \
  geode-blotter \
  asof_pins_the_tile_and_leaves_the_frame_alone

# A pinned tile does not follow the frame's as-of counter.
run_mutation "asof-pin: a pinned tile does not follow the frame's as-of" \
  crates/geode-blotter/src/tile.rs \
  '            || (matches!(self.tile_as_of, TileAsOf::Follow) && versions.as_of != now.as_of)' \
  '            || versions.as_of != now.as_of' \
  geode-blotter \
  a_pinned_tile_ignores_the_frames_as_of_and_answers_the_barrier

# The record carries as_of only while pinned.
run_mutation "asof-pin: the session writes as_of only while pinned" \
  crates/geode-blotter/src/tile.rs \
  '            TileAsOf::Follow => None,' \
  '            TileAsOf::Follow => Some(toml::Value::String("live".into())),' \
  geode-blotter \
  as_of_round_trips_through_the_session_record

# The pinned chip paints from the tile's own state.
run_mutation "asof-pin: the pinned chip paints" \
  crates/geode-blotter/src/tile.rs \
  '        if let TileAsOf::Pinned(_) = &self.tile_as_of {' \
  '        if let TileAsOf::Pinned(_) = &TileAsOf::Follow {' \
  geode-blotter \
  a_pinned_tile_paints_the_neutral_chip_and_hides_the_frame_one

# The provenance warning chip is suppressed while pinned.
run_mutation "asof-pin: the frame chip hides while pinned" \
  crates/geode-blotter/src/tile.rs \
  '            if matches!(self.tile_as_of, TileAsOf::Follow)' \
  '            if true' \
  geode-blotter \
  a_pinned_tile_paints_the_neutral_chip_and_hides_the_frame_one
```

- [ ] **Step 5: Run the new entries and the anchor check**

Run: `git add -A && git commit -q -m "wip: locality harness entries" && zsh scripts/mutation-check.sh "asof-pin" && zsh scripts/mutation-check.sh "locality:" && zsh scripts/mutation-check.sh --anchors-only`
Expected: every entry `caught`; anchors-only exits 0. (Commit BEFORE mutating — the harness restores files with `git checkout`.) Then squash the wip commit into the real one below with `git reset --soft HEAD~1`.

- [ ] **Step 6: Commit**

```bash
git add crates/geode-blotter/src/tile.rs scripts/mutation-check.sh
git commit -m "blotter: sweep test — every : line leaves the frame alone; seven harness entries

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

### Task 5: Diagnostics tile — `:level`/`:overlay` become refusals

**Files:**
- Modify: `crates/geode-diagnostics/src/commands.rs` (enum line 40–44, `LEVELS`/hints/`known_targets` lines 46–75, `parse` lines 82–123, `completions` lines 150–181, tests), `crates/geode-diagnostics/src/tile.rs` (`command` lines 598–615; the test `level_ingest_debug_changes_the_entity_and_queues_a_persist` ~line 1031), `crates/geode-diagnostics/src/lib.rs` line 3 doc, `scripts/mutation-check.sh` (near the two diagnostics `commands.rs` entries at lines 6279/6285 — read them first; they may anchor on lines this task deletes).

**Interfaces:**
- Produces: `Command::Refused(&'static str)`; `pub const COMMANDS: [&str; 1] = ["section"]`; `pub const REFUSED_LEVEL`, `REFUSED_OVERLAY`. Deleted: `Command::Level`, `Command::Overlay`, `LEVELS`, `known_targets`, `levels_hint`, `targets_hint`, and the `use geode_core::log::Level` import.

- [ ] **Step 1: Read the two existing harness entries**

Run: `sed -n 6270,6295p scripts/mutation-check.sh`. If either anchors a `level` line, rewrite it in Step 6 to anchor the `section` parse (`Some("section") => {` → mutate the unknown-section error into `Ok(Command::Section(Section::Log))`, test `section_parses_each_name_and_rejects_unknown`) rather than deleting it.

- [ ] **Step 2: Write the failing tests**

In `commands.rs` tests, delete `level_parses_target_and_level`, `overlay_parses`, `every_log_target_suffix_is_a_known_level_target`, `completions_offer_sections_then_targets_then_levels`, `completions_for_an_unknown_level_target_are_empty`; change `completions_at_the_start_offer_the_three_commands` to expect `["section"]` and rename it `completions_at_the_start_offer_section_alone`. Add:

```rust
    /// Command-line locality (2026-09-20): `level` and `overlay` are
    /// app-wide and left the line as refusals naming their door.
    #[test]
    fn level_and_overlay_are_refusals_and_not_completions() {
        assert_eq!(parse("level ingest debug"), Ok(Command::Refused(REFUSED_LEVEL)));
        assert_eq!(parse("level"), Ok(Command::Refused(REFUSED_LEVEL)));
        assert_eq!(parse("overlay"), Ok(Command::Refused(REFUSED_OVERLAY)));
        assert_eq!(completions("", 0), vec!["section"]);
        assert!(completions("level ", 6).is_empty());
    }
```

In `tile.rs` tests, replace `level_ingest_debug_changes_the_entity_and_queues_a_persist` with:

```rust
    /// The rule (command-line locality spec §2): a `:` line on this tile
    /// changes only this tile — never the app's log levels, the overlay
    /// or the frame. Every accepted word plus every refusal.
    #[gpui::test]
    fn every_colon_command_leaves_the_app_alone(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        let lines = ["section log", "section perf", "level ingest debug", "overlay"];
        for word in commands::COMMANDS {
            assert!(
                lines.iter().any(|l| l.split_whitespace().next() == Some(word)),
                "no sweep line for `:{word}`"
            );
        }
        let before = h.frame.read_with(&vcx, |f, _| f.versions());
        for line in lines {
            let result = h.tile.update(&mut vcx, |t, cx| t.command(line, cx));
            if line.starts_with("level") {
                assert_eq!(result, Err(commands::REFUSED_LEVEL.to_string()));
            } else if line == "overlay" {
                assert_eq!(result, Err(commands::REFUSED_OVERLAY.to_string()));
            } else {
                result.unwrap();
            }
            let (level, overlay) = h.diagnostics.update(&mut vcx, |d, _| {
                (d.take_pending_level(), d.take_pending_overlay_toggle())
            });
            assert!(level.is_none(), "`:{line}` queued a log-level change");
            assert!(!overlay, "`:{line}` queued an overlay toggle");
            let after = h.frame.read_with(&vcx, |f, _| f.versions());
            assert_eq!(
                (after.scope, after.grouping, after.as_of),
                (before.scope, before.grouping, before.as_of),
                "`:{line}` moved the frame"
            );
        }
    }
```

(`commands` is the module path as `tile.rs` already imports it — check the existing `commands::parse` call at line 599.)

- [ ] **Step 3: Run to verify failure**

Run: `cargo test -p geode-diagnostics 2>&1 | grep -E "^error" | head -3`
Expected: `REFUSED_LEVEL`/`COMMANDS` not found.

- [ ] **Step 4: Implement the parser**

Replace the enum and everything from `const LEVELS` through `fn targets_hint` with:

```rust
/// A parsed `:` line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Section(Section),
    /// A word this line no longer runs because it was app-wide
    /// (command-line locality spec §5): the message names the door.
    Refused(&'static str),
}

/// The words `completions` offers at the start of a line; the tile's
/// sweep test reads it.
pub const COMMANDS: [&str; 1] = ["section"];

pub const REFUSED_LEVEL: &str = "log levels are app-wide — Set log level… in the palette";
pub const REFUSED_OVERLAY: &str =
    "the overlay is app-wide — Toggle performance overlay (mod+shift+p)";
```

Delete `use geode_core::log::Level;`. In `parse`, replace the `Some("level")` and `Some("overlay")` arms with:

```rust
        Some("level") => Ok(Command::Refused(REFUSED_LEVEL)),
        Some("overlay") => Ok(Command::Refused(REFUSED_OVERLAY)),
```

In `completions`, replace the match with:

```rust
    match words.as_slice() {
        [] => COMMANDS.iter().map(|c| (*c).to_string()).collect(),
        ["section"] => Section::ALL.iter().map(|s| s.name().to_string()).collect(),
        _ => Vec::new(),
    }
```

Update the module doc's first line to: ``//! The diagnostics tile's `:` line (Phase 4b Task 5, spec §4.6; command-line locality 2026-09-20): `:section <name>`. `:level` and `:overlay` were app-wide and are refusals now (`REFUSED_LEVEL`/`REFUSED_OVERLAY`); the palette's `Set log level…` and `Toggle performance overlay` are their doors.``

- [ ] **Step 5: Implement the tile arm**

In `tile.rs` `command`, replace the `Command::Level { .. }` and `Command::Overlay` arms with `Command::Refused(message) => return Err(message.to_string()),`. In `lib.rs` line 3 change `` `:section`/`:level`/`:overlay` `` to `` `:section` (`:level`/`:overlay` moved to the palette 2026-09-20) ``. Grep the crate for `Level` imports that are now unused (`cargo clippy -p geode-diagnostics --all-targets -- -D warnings`) and remove them.

- [ ] **Step 6: Harness entries**

Fix the two existing diagnostics entries if Step 1 found they anchor deleted lines, then append:

```zsh
# Command-line locality (2026-09-20): `:level` is a refusal, never a
# log-level change.
run_mutation "locality: :level never reaches the Diagnostics entity" \
  crates/geode-diagnostics/src/tile.rs \
  '            Command::Refused(message) => return Err(message.to_string()),' \
  '            Command::Refused(_) => { self.diagnostics.update(cx, |d, cx| { d.request_overlay_toggle(); cx.notify(); }); }' \
  geode-diagnostics \
  every_colon_command_leaves_the_app_alone
```

- [ ] **Step 7: Run**

Run: `cargo test -p geode-diagnostics 2>&1 | tail -3 && git add -A && git commit -q -m wip && zsh scripts/mutation-check.sh "locality: :level" && zsh scripts/mutation-check.sh --anchors-only && git reset --soft HEAD~1`
Expected: tests pass; the entry is caught; anchors exit 0.

- [ ] **Step 8: Commit**

```bash
git add crates/geode-diagnostics scripts/mutation-check.sh
git commit -m "diagnostics: :level and :overlay are refusals; the tile's sweep test (locality §5)

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

### Task 6: Shell — `Set scope expression…` dialog, the clickable expression chip, the status-bar copy

**Files:**
- Create: `crates/geode-shell/src/shell/scope_expr_view.rs`
- Create: `crates/geode-shell/src/shell/tests/scope_expr.rs`
- Modify: `crates/geode-shell/src/shell/mod.rs` (module list near the other `mod asof_view;`; field beside `as_of_dialog` line 931; initialiser line 1642; `close_modal` line 1676; the `Change` subscription chain at line 1193), `crates/geode-shell/src/shell/input.rs` (dispatch, after the `frame::as_of` arm at line 392), `crates/geode-shell/src/defaults.rs` (registration after `frame::as_of_undo` ~line 472), `crates/geode-shell/src/shell/toolbar.rs` (signature line 164–170, expression chip lines 281–298), `crates/geode-shell/src/shell/render.rs` (closures ~line 1040, the `toolbar::toolbar(` call ~line 1062), `crates/geode-shell/src/shell/status.rs` line 222, `crates/geode-shell/src/shell/tests/mod.rs` (add `mod scope_expr;`), `scripts/mutation-check.sh`.

**Interfaces:**
- Produces: action id `frame::scope_expression` (title `Set scope expression…`, category `Frame`); `scope_expr_view::{ScopeExprState, open, commit_text}`; `ShellView.scope_expr_dialog: Option<ScopeExprState>`; selectors `scope-expr-error`, `scope-expr-hints`; the toolbar's `on_expr` parameter; the expression chip's tooltip now names `frame::scope_expression`.

- [ ] **Step 1: Write the failing tests**

`crates/geode-shell/src/shell/tests/scope_expr.rs`:

```rust
//! The scope expression dialog (command-line locality spec §4.1): the
//! palette door, a commit through `Frame::set_scope` (so undo sees it),
//! the inline parse error, the empty commit that clears, the expression
//! chip's click (typing after the click must land — the mouse-opened
//! dialog rule), and focus returning to the text field.

use super::*;
use geode_core::scope::{Scope, parse_expr};

fn expr_scope(text: &str) -> Scope {
    let mut scope = Scope::default();
    scope.expression = Some(parse_expr(text).unwrap());
    scope
}

#[gpui::test]
fn typing_an_expression_and_enter_sets_it_through_set_scope(cx: &mut gpui::TestAppContext) {
    let (shell, mut vcx) = dialog_test_shell(cx, "frame::scope_expression");
    assert!(shell.read_with(&vcx, |s, _| s.modal.is_some()));
    assert!(dialog_filter_is_focused(&shell, &mut vcx));
    vcx.simulate_input("book = 'BK000'");
    vcx.simulate_keystrokes("enter");
    let frame = shell.read_with(&vcx, |s, _| s.frame().clone());
    assert_eq!(
        frame.read_with(&vcx, |f, _| f.scope().expression.as_ref().map(ToString::to_string)),
        Some("book = 'BK000'".to_string())
    );
    assert!(shell.read_with(&vcx, |s, _| s.modal.is_none()));
    dispatch_action(&shell, "frame::scope_undo", &mut vcx);
    assert!(
        frame.read_with(&vcx, |f, _| f.scope().expression.is_none()),
        "the commit went through set_scope, so undo restores"
    );
}

#[gpui::test]
fn a_parse_error_paints_inline_and_leaves_the_frame_alone(cx: &mut gpui::TestAppContext) {
    let (shell, mut vcx) = dialog_test_shell(cx, "frame::scope_expression");
    vcx.simulate_input("book =");
    vcx.simulate_keystrokes("enter");
    vcx.run_until_parked();
    assert!(shell.read_with(&vcx, |s, _| s.modal.is_some()), "stays open");
    assert!(vcx.debug_bounds("scope-expr-error").is_some());
    let frame = shell.read_with(&vcx, |s, _| s.frame().clone());
    assert!(frame.read_with(&vcx, |f, _| f.scope().expression.is_none()));
    // Typing again clears the error.
    vcx.simulate_input(" 'x'");
    vcx.run_until_parked();
    assert!(vcx.debug_bounds("scope-expr-error").is_none());
}

#[gpui::test]
fn the_field_opens_seeded_and_an_empty_commit_clears(cx: &mut gpui::TestAppContext) {
    let (window, mut vcx) = open_shell(cx, test_services());
    let shell = shell_of(&window, &mut vcx);
    let frame = shell.read_with(&vcx, |s, _| s.frame().clone());
    frame.update(&mut vcx, |f, cx| {
        f.set_scope(expr_scope("book = 'BK000'"));
        cx.notify();
    });
    dispatch_action(&shell, "frame::scope_expression", &mut vcx);
    assert_eq!(
        shell.read_with(&vcx, |s, cx| s.dialog_input.read(cx).value().to_string()),
        "book = 'BK000'",
        "seeded with the frame's expression"
    );
    vcx.update(|window, cx| {
        let input = shell.read(cx).dialog_input.clone();
        input.update(cx, |i, cx| i.set_value("", window, cx));
    });
    vcx.simulate_keystrokes("enter");
    assert!(frame.read_with(&vcx, |f, _| f.scope().expression.is_none()));
    assert!(shell.read_with(&vcx, |s, _| s.modal.is_none()));
}

#[gpui::test]
fn clicking_the_expression_chip_opens_the_dialog_and_typing_lands(cx: &mut gpui::TestAppContext) {
    let (window, mut vcx) = open_shell(cx, test_services());
    let shell = shell_of(&window, &mut vcx);
    let frame = shell.read_with(&vcx, |s, _| s.frame().clone());
    frame.update(&mut vcx, |f, cx| {
        f.set_scope(expr_scope("book = 'BK000'"));
        cx.notify();
    });
    vcx.run_until_parked();
    let chip = vcx
        .debug_bounds("scope-expr-chip")
        .expect("the expression chip paints");
    vcx.simulate_click(chip.center(), gpui::Modifiers::default());
    vcx.run_until_parked();
    assert!(shell.read_with(&vcx, |s, _| s.scope_expr_dialog.is_some()));
    vcx.simulate_input(" and lhu = 'L1'");
    vcx.run_until_parked();
    assert_eq!(
        shell.read_with(&vcx, |s, cx| s.dialog_input.read(cx).value().to_string()),
        "book = 'BK000' and lhu = 'L1'",
        "typing after the click reaches the field"
    );
}

#[gpui::test]
fn opened_from_the_text_field_focus_returns_to_it(cx: &mut gpui::TestAppContext) {
    let (window, mut vcx) = open_shell(cx, test_services());
    let shell = shell_of(&window, &mut vcx);
    dispatch_action(&shell, "frame::focus_text", &mut vcx);
    assert!(filter_is_focused(&shell, &mut vcx));
    dispatch_action(&shell, "frame::scope_expression", &mut vcx);
    vcx.simulate_keystrokes("escape");
    vcx.run_until_parked();
    assert!(shell.read_with(&vcx, |s, _| s.modal.is_none()));
    assert!(filter_is_focused(&shell, &mut vcx), "focus went back to the field");
}
```

Add `mod scope_expr;` to `tests/mod.rs`'s list (alphabetical: after `mod scopebar;`).

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p geode-shell scope_expr 2>&1 | grep -E "^error" | head -3`
Expected: `scope_expr_dialog` not found.

- [ ] **Step 3: The new module**

`crates/geode-shell/src/shell/scope_expr_view.rs`:

```rust
//! The scope expression dialog (command-line locality spec §4.1): the
//! typed door onto the frame's expression layer now that `:scope <expr>`
//! is a refusal. In [`asof_view`](super::asof_view)'s mould: the shared
//! `dialog_input` IS the value being edited, `enter` commits, a parse
//! error paints inline and keeps the dialog open, an empty field clears
//! the expression. Opened by `frame::scope_expression` (the palette's
//! "Set scope expression…") and by a click on the scope bar's expression
//! chip (`toolbar::toolbar`'s `on_expr`).
//!
//! No dataset validation here (spec §4.1): `:scope <expr>` validated
//! against the typing tile's dataset, arbitrary for a frame-wide value;
//! the compiler drops a conjunct naming a column a dataset has no
//! storage for, and a saved scope's expression arrives unvalidated too.
//!
//! [`open`] is the only entry point and the only place a
//! [`ScopeExprState`] is constructed — nothing survives a close/reopen.

use std::rc::Rc;

use gpui::prelude::*;
use gpui::{AnyElement, App, Context, Window, div};
use gpui_component::{ActiveTheme as _, v_flex};

use geode_core::scope::{Expr, parse_expr};

use crate::keymap::{Keystroke, Modifiers};

use super::ShellView;
use super::dialog;
use super::picker::{Hint, hint_row};
use super::scale;

// ---------------------------------------------------------------------
// Pure core — no gpui.
// ---------------------------------------------------------------------

/// The dialog's state: only the last failed commit's message.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ScopeExprState {
    pub error: Option<String>,
}

/// What `enter` does with the field's text: empty clears the expression
/// (`Ok(None)`), anything else must parse. The message is the one the
/// `:` line used to show (`"{message} at column {caret}"`).
pub fn commit_text(text: &str) -> Result<Option<Expr>, String> {
    let text = text.trim();
    if text.is_empty() {
        return Ok(None);
    }
    parse_expr(text)
        .map(Some)
        .map_err(|e| format!("{} at column {}", e.message, e.caret + 1))
}

// ---------------------------------------------------------------------
// gpui: the modal.
// ---------------------------------------------------------------------

const WIDTH: f32 = 640.0;

const HINTS: &[Hint] = &[
    Hint::Key("enter"),
    Hint::Text("set · empty clears ·"),
    Hint::Key("escape"),
    Hint::Text("close"),
];

/// Open the dialog seeded with the frame's current expression source. A
/// no-op if a modal is already open, like every other `open` here. The
/// seed is written AFTER the door (`open_shell_dialog_with_key` resets
/// the field to empty), and `set_value` emits no `Change`, so the state
/// starts with no error regardless.
pub fn open(view: &mut ShellView, window: &mut Window, cx: &mut Context<ShellView>) {
    if view.modal.is_some() {
        return;
    }
    view.scope_expr_dialog = Some(ScopeExprState::default());
    dialog::open_shell_dialog_with_key(
        view,
        window,
        cx,
        "Scope expression",
        |shell, window, cx| build(shell, window, cx),
        Some(Rc::new(handle_key)),
        true,
    );
    let seed = view
        .frame
        .read(cx)
        .scope()
        .expression
        .as_ref()
        .map(ToString::to_string)
        .unwrap_or_default();
    view.dialog_input
        .update(cx, |input, cx| input.set_value(seed, window, cx));
}

/// Typing clears the last error (the `Change` subscription arm in
/// `shell/mod.rs` calls this).
pub(crate) fn on_query_changed(state: &mut ScopeExprState) {
    state.error = None;
}

fn handle_key(
    shell: &mut ShellView,
    ks: &Keystroke,
    window: &mut Window,
    cx: &mut Context<ShellView>,
) -> bool {
    if ks.mods != Modifiers::NONE || ks.key != "enter" {
        return false;
    }
    let text = shell.dialog_input.read(cx).value().to_string();
    match commit_text(&text) {
        Ok(expression) => {
            shell.frame.update(cx, |f, cx| {
                let mut scope = f.scope().clone();
                scope.expression = expression;
                if f.set_scope(scope) {
                    cx.notify();
                }
            });
            shell.close_modal(window, cx);
        }
        Err(message) => {
            if let Some(state) = shell.scope_expr_dialog.as_mut() {
                state.error = Some(message);
            }
            cx.notify();
        }
    }
    true
}

fn build(shell: &ShellView, _window: &mut Window, cx: &mut App) -> AnyElement {
    let Some(state) = shell.scope_expr_dialog.as_ref() else {
        return div().into_any_element();
    };
    let theme = cx.theme();
    let mut column = v_flex()
        .gap_2()
        .w(scale::design(WIDTH))
        .child(dialog::filter_row(&shell.dialog_input, None, cx));
    if let Some(err) = &state.error {
        column = column.child(
            div()
                .text_sm()
                .text_color(theme.danger)
                .debug_selector(|| "scope-expr-error".to_string())
                .child(err.clone()),
        );
    }
    column
        .child(hint_row(
            HINTS,
            "scope-expr-hints",
            WIDTH,
            theme.muted_foreground,
            theme.muted,
            theme.border,
            theme.radius,
        ))
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_clears_and_a_broken_expression_names_the_column() {
        assert_eq!(commit_text("   ").unwrap(), None);
        assert!(commit_text("book = 'BK000'").unwrap().is_some());
        let err = commit_text("book =").unwrap_err();
        assert!(err.contains("at column"), "{err}");
    }
}
```

Check `hint_row`'s real signature in `picker.rs` (~line 655) and match its parameter order; the `choicedialog::build` call at the end of that file is the reference.

- [ ] **Step 4: Wire the shell**

`shell/mod.rs`:
- Add `pub mod scope_expr_view;` (or `mod`, matching how `asof_view` is declared).
- Field after `as_of_dialog`: `scope_expr_dialog: Option<scope_expr_view::ScopeExprState>,`; initialiser `scope_expr_dialog: None,`; in `close_modal` add `self.scope_expr_dialog = None;`.
- In the `Change` subscription chain, after the `as_of_dialog` arm's closing brace add:

```rust
            } else if let Some(state) = view.scope_expr_dialog.as_mut() {
                // The field IS the value (spec §4.1); typing clears the last
                // failed commit's message.
                scope_expr_view::on_query_changed(state);
```

`shell/input.rs`, after the `frame::as_of` arm:

```rust
        } else if action.0 == "frame::scope_expression" {
            // Palette-only (command-line locality spec §4.1): the typed door
            // onto the frame's expression layer — the same door the scope
            // bar's expression chip opens.
            scope_expr_view::open(self, window, cx);
```

(add `scope_expr_view` to the `use super::{…}` list at the top).

`defaults.rs`, after the `frame::as_of_undo` registration:

```rust
    // The scope expression dialog (command-line locality spec §4.1):
    // palette-only, the typed door onto the frame's expression layer now
    // that `:scope <expr>` is a refusal. `…` because it opens a dialog.
    action(reg, "frame::scope_expression", "Set scope expression…", "Frame");
```

If a test in `defaults.rs` counts the builtin actions (grep `assert_eq!(reg.len()` / the comment at line 726), bump the count by one and extend its comment with `+ scope expression`.

- [ ] **Step 5: The chip and the status copy**

`toolbar.rs`: add a parameter `on_expr: impl Fn(&mut Window, &mut App) + Clone + 'static,` after `on_as_of`. Replace the expression chip block (lines 281–298) with:

```rust
    if let Some(expr) = &model.expr {
        has_chips = true;
        // Clickable since 2026-09-20 (command-line locality spec §4.1): the
        // mouse form of `frame::scope_expression`. The chip pairing is the
        // selection chips' own (`chip_states`), already in `shipped()`.
        let on_expr = on_expr.clone();
        chips_row = chips_row.child(
            chip(
                "scope-expr-chip".into(),
                expr.clone(),
                chip_fg,
                chip_bg,
                chip_radius,
                || "scope-expr-chip".to_string(),
            )
            .tooltip(tips::tip_with(
                SharedString::new_static("tip-scope-expr-chip"),
                model.expr_full.clone().unwrap_or_default(),
                Some("frame::scope_expression"),
                Some(SharedString::new_static("click to edit")),
            ))
            .pointer_states(chip_states)
            .on_mouse_down(MouseButton::Left, move |_event, window, cx| {
                on_expr(window, cx)
            }),
        );
    }
```

Update the comment at line 184 ("The text, expression and contradiction chips have no listener") to name only the contradiction chip. If `chip_states` is not `Copy`, clone it where the selection chips do.

`render.rs`: beside `on_as_of` add

```rust
        // The expression chip's click (command-line locality 2026-09-20)
        // — the mouse form of `frame::scope_expression`.
        let expr_entity = cx.entity();
        let on_expr = move |window: &mut Window, cx: &mut App| {
            expr_entity.update(cx, |view, cx| {
                scope_expr_view::open(view, window, cx);
            });
        };
```

and pass `on_expr,` after `on_as_of,` in the `toolbar::toolbar(` call.

`status.rs` line 222: `format!("AS OF {t} · :live to return")` → `format!("AS OF {t} · Return to live in the palette")`. Grep `crates/geode-shell/src/shell/tests` for `to return` and update any expectation.

- [ ] **Step 6: Run**

Run: `cargo test -p geode-shell 2>&1 | tail -5 && cargo clippy -p geode-shell --all-targets -- -D warnings 2>&1 | tail -3`
Expected: all pass, clippy clean.

- [ ] **Step 7: Harness entries**

Append to `scripts/mutation-check.sh` after the locality entries:

```zsh
# An empty commit clears the frame's expression (locality §4.1).
run_mutation "expr-dialog: an empty commit clears the expression" \
  crates/geode-shell/src/shell/scope_expr_view.rs \
  '        return Ok(None);' \
  '        return Err("empty".into());' \
  geode-shell \
  the_field_opens_seeded_and_an_empty_commit_clears

# A commit goes through set_scope, so undo restores it.
run_mutation "expr-dialog: the commit goes through set_scope" \
  crates/geode-shell/src/shell/scope_expr_view.rs \
  '                if f.set_scope(scope) {' \
  '                if f.set_scope_in_session(scope) {' \
  geode-shell \
  typing_an_expression_and_enter_sets_it_through_set_scope
```

(Check `Frame::set_scope_in_session` bypasses the undo stack — its doc at `frame.rs:313`; if it does not, mutate the commit into `f.clear_scope()` instead.) Run them and `--anchors-only` after a wip commit, as in Task 4 Step 5.

- [ ] **Step 8: Commit**

```bash
git add crates/geode-shell scripts/mutation-check.sh
git commit -m "shell: Set scope expression… dialog, clickable expression chip, status copy (locality §4.1)

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

### Task 7: Shell — `Set log level…` as a two-step choice dialog

**Files:**
- Modify: `crates/geode-shell/src/shell/choicedialog.rs` (`Target` line 68, constructors after `tile_kinds` ~line 121, `pick_at` line 132, `Pick` line 163, `chrome` line 225, a new `open_log_level` after `open_tile_kinds`, `commit` line 267, `handle_key`'s `Cancel` arm line 306, tests), `crates/geode-shell/src/shell/input.rs` (dispatch), `crates/geode-shell/src/defaults.rs` (registration), `crates/geode-shell/src/shell/tests/diagnostics.rs` (new tests), `scripts/mutation-check.sh`.

**Interfaces:**
- Produces: action id `log::level` (title `Set log level…`, category `Diagnostics`); `Target::LogLevel { targets: Vec<String>, chosen: Option<String> }`; `Pick::LogTarget(String)`, `Pick::LogLevel(String, Level)`; `ChoiceDialogState::log_targets(levels: &LogLevels) -> Self`, `ChoiceDialogState::log_levels(target: String, current: Level) -> Self`; `pub const LEVEL_WORDS: [(&str, Level); 5]`; `pub fn open_log_level(view, window, cx)`; selectors `loglevel-choice-list`, `loglevel-choice-{text}`, `loglevel-hints`.

- [ ] **Step 1: Write the failing tests**

Pure, in `choicedialog.rs` tests:

```rust
    /// Step 1 rows are the seven `geode::` suffixes with each one's
    /// effective level; step 2 rows are the five levels with the current
    /// one lit.
    #[test]
    fn log_level_rows_name_targets_then_levels() {
        let levels = geode_core::log::LogLevels {
            default: geode_core::log::Level::INFO,
            targets: vec![("ingest".into(), geode_core::log::Level::DEBUG)],
        };
        let state = ChoiceDialogState::log_targets(&levels);
        assert_eq!(state.list.options()[0], "ingest · debug");
        assert_eq!(state.list.options()[1], "query · info");
        assert_eq!(state.list.options().len(), geode_core::log::TARGETS.len());
        assert_eq!(state.highlighted_pick(), Some(Pick::LogTarget("ingest".into())));
        assert_eq!(state.jump("1"), None, "digits type on this target");

        let mut state = ChoiceDialogState::log_levels("ingest".into(), geode_core::log::Level::DEBUG);
        assert_eq!(state.list.options(), ["error", "warn", "info", "debug", "trace"]);
        assert_eq!(
            state.highlighted_pick(),
            Some(Pick::LogLevel("ingest".into(), geode_core::log::Level::DEBUG)),
            "opens on the current level"
        );
        state.list.set_query("tr");
        assert_eq!(
            state.pick_at_ranked(0),
            Some(Pick::LogLevel("ingest".into(), geode_core::log::Level::TRACE))
        );
    }
```

Window, in `crates/geode-shell/src/shell/tests/diagnostics.rs` (append):

```rust
/// `Set log level…` (command-line locality spec §4.2): target, then
/// level, landing on `Diagnostics::request_level` — the path `:level`
/// used to take. `escape` on the level step returns to the target step.
#[gpui::test]
fn set_log_level_picks_a_target_then_a_level(cx: &mut gpui::TestAppContext) {
    let (shell, mut vcx) = dialog_test_shell(cx, "log::level");
    assert!(vcx.debug_bounds("loglevel-choice-list").is_some());
    vcx.simulate_input("ingest");
    vcx.simulate_keystrokes("enter");
    vcx.run_until_parked();
    assert!(shell.read_with(&vcx, |s, _| s.modal.is_some()), "step 2 is open");
    assert!(vcx.debug_bounds("loglevel-choice-debug").is_some());
    assert_eq!(
        shell.read_with(&vcx, |s, cx| s.dialog_input.read(cx).value().to_string()),
        "",
        "the field is reset between steps"
    );

    vcx.simulate_keystrokes("escape");
    vcx.run_until_parked();
    assert!(shell.read_with(&vcx, |s, _| s.modal.is_some()), "back on step 1");
    assert!(vcx.debug_bounds("loglevel-choice-ingest · info").is_some());

    vcx.simulate_input("ingest");
    vcx.simulate_keystrokes("enter");
    vcx.simulate_input("debug");
    vcx.simulate_keystrokes("enter");
    vcx.run_until_parked();
    assert!(shell.read_with(&vcx, |s, _| s.modal.is_none()));
    let diagnostics = shell.read_with(&vcx, |s, _| s.diagnostics().clone());
    let levels = diagnostics.read_with(&vcx, |d, _| d.levels.clone());
    assert_eq!(
        levels.targets,
        vec![("ingest".to_string(), geode_core::log::Level::DEBUG)]
    );
}
```

(`dialog_test_shell` dispatches the action and draws; check its preamble at `tests/mod.rs:466` for whether it asserts `modal.is_some()` itself.) The default level in `test_services()`'s `Diagnostics` is `LogLevels::default()`; check `default` is `INFO` there, else adjust the `"ingest · info"` expectation.

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p geode-shell log_level 2>&1 | grep -E "^error" | head -3`
Expected: `log_targets` not found.

- [ ] **Step 3: The pure core**

In `choicedialog.rs`, extend `Target` and `Pick`:

```rust
pub enum Target {
    Grouping { slots: Vec<Option<u8>> },
    TileKind { kinds: Vec<String> },
    /// `Set log level…` (command-line locality spec §4.2), two steps over
    /// one dialog: `chosen` is `None` while the rows are targets and
    /// `Some(target)` while they are levels.
    LogLevel {
        targets: Vec<String>,
        chosen: Option<String>,
    },
}

pub enum Pick {
    Slot(Option<u8>),
    Kind(String),
    /// Step 1 of `Set log level…`: replace the rows with the levels.
    LogTarget(String),
    /// Step 2: `Diagnostics::request_level`.
    LogLevel(String, Level),
}
```

Add `use geode_core::log::{Level, LogLevels, TARGETS};` and:

```rust
/// The level rows, in severity order, as `[log]` spells them.
pub const LEVEL_WORDS: [(&str, Level); 5] = [
    ("error", Level::ERROR),
    ("warn", Level::WARN),
    ("info", Level::INFO),
    ("debug", Level::DEBUG),
    ("trace", Level::TRACE),
];

fn level_word(level: Level) -> &'static str {
    LEVEL_WORDS
        .iter()
        .find(|(_, l)| *l == level)
        .map(|(w, _)| *w)
        .unwrap_or("info")
}

/// A target's effective level: its own entry, else the default.
fn effective_level(levels: &LogLevels, target: &str) -> Level {
    levels
        .targets
        .iter()
        .find(|(t, _)| t == target)
        .map(|(_, l)| *l)
        .unwrap_or(levels.default)
}
```

Constructors on `ChoiceDialogState`:

```rust
    /// Step 1 of `Set log level…`: one row per `geode::` target suffix,
    /// `"{target} · {level}"`, the highlight on the first.
    pub fn log_targets(levels: &LogLevels) -> Self {
        let targets: Vec<String> = TARGETS
            .iter()
            .map(|t| t.strip_prefix("geode::").unwrap_or(t).to_string())
            .collect();
        let options = targets
            .iter()
            .map(|t| format!("{t} · {}", level_word(effective_level(levels, t))))
            .collect();
        Self {
            list: ChoiceList::new(options, choice::DEFAULT_CAP),
            target: Target::LogLevel {
                targets,
                chosen: None,
            },
        }
    }

    /// Step 2: the five levels, the highlight placed on `current` so a
    /// bare `enter` changes nothing.
    pub fn log_levels(target: String, current: Level) -> Self {
        let options: Vec<String> = LEVEL_WORDS.iter().map(|(w, _)| (*w).to_string()).collect();
        let mut list = ChoiceList::new(options, choice::DEFAULT_CAP);
        list.place(Some(level_word(current)));
        Self {
            list,
            target: Target::LogLevel {
                targets: Vec::new(),
                chosen: Some(target),
            },
        }
    }
```

`pick_at`:

```rust
            Target::LogLevel { targets, chosen } => match chosen {
                None => Pick::LogTarget(targets[declared].clone()),
                Some(target) => Pick::LogLevel(target.clone(), LEVEL_WORDS[declared].1),
            },
```

`chrome`:

```rust
        Target::LogLevel { .. } => ("Log level", "loglevel", "loglevel-hints", LOG_HINTS),
```

with

```rust
const LOG_HINTS: &[Hint] = &[
    Hint::Text("type to filter ·"),
    Hint::Key("up"),
    Hint::Key("down"),
    Hint::Text("move ·"),
    Hint::Key("enter"),
    Hint::Text("choose ·"),
    Hint::Key("escape"),
    Hint::Text("back / close"),
];
```

`highlighted_slot` gains `Pick::LogTarget(_) | Pick::LogLevel(..) => None`.

- [ ] **Step 4: The doors, the commit and the escape step**

After `open_tile_kinds`:

```rust
/// Open `Set log level…` on the target step (`log::level`, palette-only).
pub fn open_log_level(view: &mut ShellView, window: &mut Window, cx: &mut Context<ShellView>) {
    let state = ChoiceDialogState::log_targets(&view.diagnostics.read(cx).levels);
    open(view, state, window, cx);
}
```

In `commit`:

```rust
        Pick::LogTarget(target) => {
            // Step 2 replaces the rows in place; the modal stays open and
            // the field is reset (`set_value` emits no `Change`, and the
            // new list starts with an empty query).
            let current = effective_level(&shell.diagnostics.read(cx).levels, &target);
            let state = ChoiceDialogState::log_levels(target, current);
            shell
                .choice_dialog_scroll
                .scroll_to_item(state.list.ranked_highlighted());
            shell.choice_dialog = Some(state);
            let input = shell.dialog_input.clone();
            input.update(cx, |i, cx| i.set_value("", window, cx));
            input.read(cx).focus_handle(cx).focus(window, cx);
            cx.notify();
        }
        Pick::LogLevel(target, level) => {
            shell.diagnostics.update(cx, |d, cx| {
                d.request_level(&target, level);
                cx.notify();
            });
            shell.close_modal(window, cx);
        }
```

In `handle_key`, replace `Some(ChoiceKey::Cancel) => return false,` with:

```rust
        Some(ChoiceKey::Cancel) => {
            // The level step goes BACK to the target step; every other
            // dialog (and the target step itself) falls through to
            // `handle_key_down`'s modal-closes-on-escape branch.
            if matches!(
                shell.choice_dialog.as_ref().map(|s| &s.target),
                Some(Target::LogLevel { chosen: Some(_), .. })
            ) {
                let state = ChoiceDialogState::log_targets(&shell.diagnostics.read(cx).levels);
                shell
                    .choice_dialog_scroll
                    .scroll_to_item(state.list.ranked_highlighted());
                shell.choice_dialog = Some(state);
                let input = shell.dialog_input.clone();
                input.update(cx, |i, cx| i.set_value("", window, cx));
                input.read(cx).focus_handle(cx).focus(window, cx);
                cx.notify();
                return true;
            }
            return false;
        }
```

Extend the module doc's target list with a third bullet for `Target::LogLevel`.

`input.rs`, after the `tile::add` arm:

```rust
        } else if action.0 == "log::level" {
            // Palette-only (command-line locality spec §4.2): the two-step
            // log-level picker, `:level`'s replacement.
            choicedialog::open_log_level(self, window, cx);
```

`defaults.rs`, after `frame::scope_expression`:

```rust
    // Set log level… (command-line locality spec §4.2): target then level
    // over the choice dialog, landing on `Diagnostics::request_level` —
    // the diagnostics tile's `:level` moved here. Category "Diagnostics"
    // beside the tile's own actions.
    action(reg, "log::level", "Set log level…", "Diagnostics");
```

(bump the action-count test again if there is one).

- [ ] **Step 5: Run**

Run: `cargo test -p geode-shell 2>&1 | tail -5 && cargo clippy -p geode-shell --all-targets -- -D warnings 2>&1 | tail -3`
Expected: all pass.

- [ ] **Step 6: Harness entries**

```zsh
# Step 2 opens on the target's CURRENT level (locality §4.2).
run_mutation "loglevel: the level step opens on the current level" \
  crates/geode-shell/src/shell/choicedialog.rs \
  '        list.place(Some(level_word(current)));' \
  '        list.place(None);' \
  geode-shell \
  log_level_rows_name_targets_then_levels

# The pick lands on request_level, not on a no-op.
run_mutation "loglevel: the level pick reaches request_level" \
  crates/geode-shell/src/shell/choicedialog.rs \
  '                d.request_level(&target, level);' \
  '                let _ = (&target, level);' \
  geode-shell \
  set_log_level_picks_a_target_then_a_level
```

Run them and `--anchors-only` after a wip commit.

- [ ] **Step 7: Commit**

```bash
git add crates/geode-shell scripts/mutation-check.sh
git commit -m "shell: Set log level… two-step choice dialog replaces :level (locality §4.2)

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

### Task 8: Market-data sweep, the contract doc, spec notes, CLAUDE.md, history, final checks

**Files:**
- Modify: `crates/geode-marketdata/src/tile.rs` (tests), `crates/geode-shell/src/module.rs` (`command` doc, line 161), `docs/superpowers/specs/2026-09-03-geode-phase-3-blotter-design.md` (§4.3, line 403), `docs/superpowers/specs/2026-09-06-geode-phase-4-frame-features-design.md` (§3.6 line 551, §3.8 line 608, §4.6 line 1194), `docs/superpowers/specs/2026-09-20-geode-command-line-locality-design.md` (as-built notes), `CLAUDE.md` (status table after line 41; load-bearing bullet under "Shell: frame, tiles, diagnostics"; harness count in the Commands block), `docs/phase-history.md` (append).

- [ ] **Step 1: The market-data sweep test**

Append to `mod tests` in `crates/geode-marketdata/src/tile.rs` (it has `Harness { content, frame, diagnostics, .. }` and `open(cx)`; `VERBS` is `crate::commands::VERBS` — make it `pub(crate)` if it is private):

```rust
    /// The rule (command-line locality spec §2): every `:` verb the panel
    /// accepts changes only the panel — never the frame or the app.
    #[gpui::test]
    fn every_colon_command_leaves_the_frame_alone(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        let lines = [
            "underlying SPX",
            "revert",
            "bump 0.5",
            "rebase",
            "upload",
            "set spot 100",
            "auto hold",
            "menu",
        ];
        for word in crate::commands::VERBS {
            assert!(
                lines.iter().any(|l| l.split_whitespace().next() == Some(word)),
                "no sweep line for `:{word}`"
            );
        }
        let before = h.frame.read_with(&vcx, |f, _| f.versions());
        for line in lines {
            let _ = vcx.update(|window, cx| h.content.command(line, window, cx));
            let after = h.frame.read_with(&vcx, |f, _| f.versions());
            assert_eq!(
                (after.scope, after.grouping, after.as_of),
                (before.scope, before.grouping, before.as_of),
                "`:{line}` moved the frame"
            );
            let (level, overlay) = h.diagnostics.update(&mut vcx, |d, _| {
                (d.take_pending_level(), d.take_pending_overlay_toggle())
            });
            assert!(level.is_none() && !overlay, "`:{line}` reached the app");
            // Any per-line outcome is fine (a refused key, nothing to
            // revert); the rule is about what it did NOT touch.
        }
    }
```

If a line's argument shape is wrong for the panel's parser (check `commands::parse` for `underlying`/`bump`/`set`), use one its own tests use. Run: `cargo test -p geode-marketdata every_colon_command 2>&1 | tail -3`. Expected: PASS.

- [ ] **Step 2: The contract doc**

In `crates/geode-shell/src/module.rs`, replace the `command` doc line with:

```rust
    /// A `:` line, without the colon. `Err` is shown inline on the line.
    ///
    /// **The rule (command-line locality spec §2, 2026-09-20): a `:` line
    /// changes only THIS tile** — what it queries for, how it paints,
    /// its cursor, its draft. It never writes the frame (scope, grouping,
    /// as-of, slots), the shell, the config or the log levels, and never
    /// changes what another tile shows. A frame- or app-wide effect is a
    /// palette action instead (the palette is global or local per
    /// action). Every module with a vocabulary keeps a sweep test
    /// (`every_colon_command_leaves_the_frame_alone`) that runs each word
    /// and asserts the frame's counters and pending persist and the
    /// `Diagnostics` entity's pending requests are untouched; a word that
    /// used to be frame-wide stays in the parser as a REFUSAL whose
    /// message names the door.
```

- [ ] **Step 3: Spec supersession notes**

Insert one blockquote directly under each heading:

- Phase 3 §4.3 (line 403): `> **Superseded 2026-09-20** by `docs/superpowers/specs/2026-09-20-geode-command-line-locality-design.md`: a `:` line changes only its own tile. The `:scope …`, `:asof undo`, `:live` and `:group save N` rows below are refusals now, and `:asof <time>` pins the TILE (with `:asof live` / `:asof clear`), not the frame.`
- Phase 4a §3.6 (line 551): `> **Amended 2026-09-20** (command-line locality spec §3, §5): `:asof <time>` pins the tile, `:asof undo` and `:live` are refusals; the frame's as-of is set only through this selector, `frame::live` and `frame::as_of_undo`.`
- Phase 4a §3.8 (line 608): `> **Amended 2026-09-20** (command-line locality spec §4.1, §5): the `:scope` forms are refusals; the frame's expression is typed through `frame::scope_expression` ("Set scope expression…") and the scope bar's expression chip.`
- Phase 4a §4.6 (line 1194): `> **Amended 2026-09-20** (command-line locality spec §4.2, §5): `:level` and `:overlay` are refusals; `log::level` ("Set log level…") and `perf::toggle_overlay` are their doors.`

In the locality spec itself, append an `## 10. As built` section:

```markdown
## 10. As built (2026-09-20)

- §2's sweep checks `Frame::take_pending_persist` and the `Diagnostics`
  entity's pending level/overlay rather than diffing a user directory:
  those two are the only channels a module has onto a config write (the
  shell performs the write), so they are the honest check at module
  level.
- §7's `asof-pin:chip` entry became `asof-pin: the pinned chip paints`
  and `asof-pin: the frame chip hides while pinned`: a `TestAppContext`
  can see whether a chip paints, not its colour; the tone rides the
  existing `every_chip_tone_is_readable_on_every_bundled_theme` sweep.
- §5's refusal for `:asof undo` names the palette title as it is,
  `Swap to the previous as of`; `:group save N` names `Edit groupings…`.
- The status bar's as-of segment read `:live to return`; it now reads
  `Return to live in the palette`.
- The expression chip's tooltip had said `:filter <expr> sets it` (the
  tile layer); it names `frame::scope_expression` now.
- Display checks pending: §8's list.
```

- [ ] **Step 4: CLAUDE.md and history**

CLAUDE.md status table, after the Timeseries Part 1 row:

```markdown
| Command-line locality (2026-09-20) | A `:` line changes only its own tile (reversing Phase 3 §4.3): `:scope`, `:asof undo`, `:live`, `:group save`, `:level`, `:overlay` are refusals naming their door; `:asof <time>|live|clear` pins the BLOTTER tile (`TileAsOf`, neutral `AS OF`/`LIVE` chip, session `as_of`); `frame::scope_expression` (`Set scope expression…`, the expression chip's click) and `log::level` (`Set log level…`, two-step choice) are the palette doors; one sweep test per module. Display checks pending. | `2026-09-20-…command-line-locality` |
```

Load-bearing bullet, under "Shell: frame, tiles, diagnostics" after the `ModuleFactory::contexts()` bullet:

```markdown
- **A `:` line changes only its own tile (2026-09-20).** Every module's vocabulary is swept by `every_colon_command_leaves_the_frame_alone` (frame counters, `take_pending_persist`, the `Diagnostics` pending requests); a word that used to be frame-wide is `Command::Refused(&str)` naming its door and is never a completion. The blotter's `TileAsOf` is the third override beside `Pin` and `tile_scope`: `differs_on_followed` compares `as_of` only while `Follow`, `requery` reads the pin, the neutral chip paints from `tile_as_of` (cached strings via `set_tile_as_of`) and suppresses the provenance warning chip, and the session key is `as_of` (`"live"` or RFC 3339). `scope_expr_view::open` seeds the field AFTER the dialog door (the door resets it). `Target::LogLevel` is two steps in one modal: `escape` on the level step goes back, not out.
```

Update the harness count in the Commands block (`# mutation harness (N entries)`) to `grep -c "^run_mutation" scripts/mutation-check.sh`.

`docs/phase-history.md`: append one paragraph, in the file's voice, recording: the user ruling and the four decisions (override, dialog, drop `:group save`, `:level` to the palette), the refusal mechanism, `TileAsOf` and the followed-counter guard, the chip tone ruling (neutral for a chosen pin; the `LIVE` chip under a historical stripe accepted), the seed-after-door trap in `scope_expr_view::open`, the two-step escape rule, the harness entries added (name them), the tests that pin each, and the display checks pending.

- [ ] **Step 5: Final checks**

Run, in order:

```bash
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo check -p geode-shell --features test-support --all-targets
cargo test --workspace 2>&1 | tail -5
cargo bench --workspace --no-run 2>&1 | tail -2
zsh scripts/mutation-check.sh --anchors-only
zsh scripts/mutation-check.sh --changed
```

Expected: clean, all tests pass, anchors exit 0, every `--changed` entry caught.

- [ ] **Step 6: Commit**

```bash
git add -A
git commit -m "docs: command-line locality — contract doc, spec notes, CLAUDE.md, history; market-data sweep

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

Then hand off to `superpowers:finishing-a-development-branch`.
