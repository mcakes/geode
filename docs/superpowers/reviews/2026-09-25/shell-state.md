# geode-shell: pure/state layer review

Scope: `src/tiling/` (tree, workspaces, docks, dividers, dropzones), `src/keymap/`
(keystroke, context, build, matcher, fragments), `config_write.rs`, `session.rs` +
`shell/session_io.rs`, `reload.rs` + `shell/hot_reload.rs`, `perf.rs`,
`palette_usage.rs`, `keymap_edit.rs`, `log_persist.rs`, `actions.rs`, `defaults.rs`
(modifier alias), `tips.rs`, plus the watcher loop in `shell/mod.rs` and the
occupant/session seams in `shell/occupants.rs` that the state layer depends on.

## Summary

1. The pure layer really is pure: `tiling` and `keymap` have no gpui import, and
   every structural verb is reachable from a plain unit test — an unusually
   well-kept boundary for a 94k-line UI crate.
2. The strongest correctness finding is a **session round-trip hole**: `to_toml`
   writes a `Stack` node's `active` index but restoration can silently re-point
   it, and `visible_tiles()` is not what `layout()` shows under fullscreen — so
   "what was on screen" is not a round-trip invariant.
3. The keymap engine is honest about its own approximations, but binding
   resolution is **O(bindings) per keypress with a fresh `Vec<KeyContext>` and
   `String` clones per frame of the stack** — three allocations minimum on the
   hottest path in a keyboard-first app.
4. `config_write` fixes the ordered-persist race properly (directory-scoped FIFO
   + transaction lock), but **session writes deliberately bypass both**, and the
   quit hook does not join an in-flight periodic save — a documented way to lose
   the last layout change.
5. Test coverage is deep on examples and thin on invariants: ~150 tiling tests,
   zero property/fuzz tests, and no test asserts the tree invariants
   (`split.children.len() >= 2`, ratios sum to 1, one-place-per-`TileId`) hold
   after an arbitrary verb sequence.

---

## Critical

### C1. `Tree::layout` paints a fullscreen tile that is a *hidden* stack member

**Location:** `src/tiling/tree.rs:527-540` (`layout`), `tree.rs:249-266`
(`activate`), `tree.rs:812-836` (`toggle_fullscreen`)

`layout` returns `vec![(fs, bounds)]` whenever `self.contains(fs)` — and
`contains` uses `holds_anywhere`, which returns true for an *inactive* stack
member (`tree.rs:135-151`: `node_holds` checks `children.contains(&id)` for a
`Stack`, not `children[active] == id`). `activate` transfers fullscreen only when
the *outgoing* active member held it (`tree.rs:262-265`). So any path that
changes a stack's active member without going through `activate`'s
outgoing-check, or that sets fullscreen on a member and then changes the active
member by another route, leaves a hidden member fullscreen — and `layout` then
paints exactly that hidden tile over the whole region.

The code knows this: `stack_after` clears fullscreen unconditionally with a
six-line comment naming precisely this failure
(`tree.rs:305-312`: "a fullscreen held by the stack's old active member survives
hidden — `Tree::layout` paints it (any tile `contains(fs)`)"), and `pop_out`
(`tree.rs:365-367`) copies the same defence. But the defence is at the call
sites, not in the invariant: `toggle_fullscreen` (`tree.rs:812`) sets
`fullscreen = Some(focused)` with no stack check, and `Tree::from_parts`
(`tree.rs:894-925`) filters fullscreen only against `leaves` (which
`collect_leaves` fills with *all* stack members, `tree.rs:998-1011`) — so a
session file naming a hidden member as `fullscreen` restores intact and the
first render paints the wrong tile full-window.

**Impact:** a trader sees one tile filling the workspace while structural focus,
the stack marker, and every keyboard verb operate on a different tile. On a
derivatives desk that is a wrong-number class defect, not a cosmetic one.

**Direction:** make it an invariant rather than a per-call-site patch. Either
`layout` resolves `fs` through the active member (`visible_tiles().contains(fs)`
instead of `contains(fs)`), or `fullscreen` is normalised on write — a private
`set_fullscreen` that refuses/redirects a hidden member, with `from_parts`
filtering against `visible_tiles()`. The existing `stack_after`/`pop_out`
clears can then be deleted as redundant, which is the tell that the invariant
moved to the right place. Note `docs/current/tiling.md` already says
"`visible_tiles()` selects active members but does not itself apply the
fullscreen filter used by `layout`" — the doc records the asymmetry without
noticing it is exploitable.

---

## Major

### M1. Session save can lose the last layout change at quit

**Location:** `src/shell/session_io.rs:31-67` (`take_dirty_session_write`),
`session_io.rs:69-85` (`save_session`), `src/shell/mod.rs:1357-1370` (the
watcher's write), `src/session.rs:865-911`

Three separate gaps compose:
(a) `take_dirty_session_write` clears `self.session_dirty = false` at line 44,
*before* `self.services.session_path.clone()?` at line 45 — so with no configured
session path the flag is consumed and thrown away (harmless today, since nothing
will ever save, but it is the wrong order);
(b) on a serialization error (line 61-64) the baselines are deliberately not
advanced, but `session_dirty` was already cleared at line 44, so a layout-only
change loses its dirt permanently — the code comments this as known
("a layout-only change can still lose its dirty flag");
(c) the quit hook `save_session` does not join the in-flight periodic write, and
`session::write_atomic` bypasses `config_write`'s directory FIFO and transaction
lock entirely (`session.rs:889-894` calls `write_file` directly, not `submit`).
The last `rename` wins, so a periodic write that started before quit can land
*after* the shutdown snapshot.

**Impact:** the workspace layout a trader left is silently the previous one on
next launch. Rare, but it is the one piece of state the user cannot reconstruct
from config.

**Direction:** (a) move the `session_dirty = false` below the `?`; (b) restore the
flag on the serialization-error path; (c) route session writes through
`config_write::submit` on the session directory so quit-time ordering is the same
FIFO every other write uses, and have the quit hook await the queue. The
docs (`shell.md` §Saving and failure behavior) currently *document* the loss
rather than fixing it; if the ordering is deliberate, the comment should say what
makes it acceptable, not just that it happens.

### M2. Per-keypress allocation in binding resolution

**Location:** `src/shell/input.rs:35-48` (`context_stack`), `input.rs:56`,
`input.rs:806` (both call sites), `src/keymap/context.rs:9-38`
(`KeyContext`), `src/keymap/matcher.rs:34-93` (`Matcher::press`)

`context_stack` returns a fresh `Vec<KeyContext>` on every call, and each
`KeyContext` owns `flags: Vec<String>` plus `pairs: Vec<(String, String)>`
(`context.rs:12-15`) — so building the stack for one keypress allocates the outer
`Vec`, then one `Vec<String>` + one `String` per frame (`KeyContext::new` at
`context.rs:11-16` does `vec![name.into()]`), and modules add more via
`.pair("mode", "normal")` (`module.rs:706`). `handle_key_down` builds it twice on
the common path: once inside `is_palette_toggle` → `context_stack`
(`input.rs:56`) and again for ordinary matching (`input.rs:806`). Then
`Matcher::press` walks **every** binding in the keymap linearly
(`matcher.rs:61-79`), evaluating each predicate against the stack, with no
index by first keystroke.

The charter is explicit ("hot paths are allocation-free… per-frame heap churn is
a reviewable defect", PHILOSOPHY.md §6), and `tips.rs` goes to remarkable lengths
to avoid one `format!` per frame (`tips.rs:96-112`) — so this is an inconsistently
applied standard, on the single hottest path in a keyboard-first product.

**Impact:** not user-visible at today's binding counts (the shipped keymap is
small), but it is the path that must stay cheap as module fragments accumulate,
and `is_palette_toggle` doubling the work is pure waste.

**Direction:** three independent wins, cheapest first: (1) build the stack once
in `handle_key_down` and pass `&[KeyContext]` to `is_palette_toggle`; (2) make
`KeyContext` borrow (`&'static str` for flags/keys, which every call site already
supplies as a literal — `KeyContext::new("workspace")`, `.pair("mode", "normal")`)
or intern them; (3) index `Keymap` by first keystroke so `press` scans candidates,
not the whole list. (1) is a two-line change and should happen regardless.

### M3. `effective_binding`/`user_overrides_for` are O(n²) and run per rendered row

**Location:** `src/keymap/build.rs:176-233` (`effective_binding`,
`user_overrides_for`, `is_shadowed`), `src/shell/keybindings_view.rs:99-110`

`is_shadowed` (`build.rs:215-220`) scans `bindings[index+1..]` for every
candidate, and `effective_binding` calls it for every binding matching the action
— so resolution is quadratic in the binding list. `keybindings_view.rs:99` calls
`effective_binding` and `:110` calls `user_overrides_for` **per action row**,
making the dialog O(actions x bindings²). `user_overrides_for` is worse: it
allocates a `lower: Vec<&Binding>` (`build.rs:196`) per call and runs a nested
`any` over it (`build.rs:200-206`).

`tips::chord_for` (`tips.rs:50-57`) also calls `effective_binding`, but honestly
notes it "runs only inside a hover closure, never per frame" — the dialog has no
such excuse.

**Impact:** the keybindings dialog is the surface a trader uses to *understand*
their bindings; it should not get slower as they add overrides. Currently
survivable only because the action list is short.

**Direction:** compute the shadow set once — a single reverse pass building a
`HashMap<(sequence, context_source), winning_index>` — and hand it to both
helpers, or precompute the whole action→effective-binding map once per keymap
rebuild and cache it beside `Chords`.

### M4. Session round-trip is not actually a round-trip for stack activity

**Location:** `src/session.rs:726-782` (`node_to_toml`), `session.rs:784-863`
(`node_from_toml`), `src/tiling/tree.rs:927-996` (`validate_node`)

`node_to_toml` writes `active` verbatim (`session.rs:750`), and `node_from_toml`
reads it with `.max(0) as usize` (`session.rs:855-858`), leaving validation to
`validate_node`. But `validate_node` *prunes* stack members that were already
`seen` elsewhere in tree order (`tree.rs:713-719` equivalent, at
`tree.rs:932-947`) and only then checks `active < n`, resetting to `0` otherwise.
So a duplicate member earlier in the tree silently shifts which member is active
— and there is no warning: the pruning is inside `Tree::from_parts`'s
`Result<Option<Node>>` path, which returns no diagnostics, while the *dock*
duplicate-claim healing does warn (`workspaces.rs:656-683`). Similarly
`Workspaces::from_parts` explicitly does **not** check plain duplicate leaf IDs
across main trees (`workspaces.rs:861-875` doc: "This does not check plain
duplicate IDs across main trees"), and `tiling.md` confirms "Live operations
assume unique IDs."

So: a session file with a duplicated `TileId` in two main trees restores into a
state every live verb assumes cannot exist, with no warning, and the
`debug_assert!`s in `drop_split`/`drop_stack`/`drop_to_dock`
(`workspaces.rs:545-552`, `:597-604`, `:632-639`) are the only thing that notices
— in debug builds only.

**Impact:** silent wrong-tile behaviour after a hand-edited or
concurrently-written session file. The healing infrastructure exists and is good;
it just stops one case short.

**Direction:** extend the cross-workspace claim pass in `Workspaces::from_parts`
to seed `claimed` from main trees *incrementally* (so the second main tree's
duplicate is pruned like a dock's), and make `validate_node`'s stack pruning
return warnings the way dock healing does. Add a round-trip test that asserts
`visible_tiles()` and `stack_position()` for every tile survive
save→load, not just that the tree shape does.

### M5. `session_dirty` is set by pointer paths but not by every keyboard path

**Location:** `src/shell/input.rs:113,133,151`, `shell/add_tile.rs:56,70,99,168`,
`shell/drag.rs:240,318,399,663`, `shell/render.rs:711,739,798,821`,
`shell/occupants.rs:448,516`

`apply_workspace_action` is the single router for every pure layout verb
(`workspaces.rs:925-1028`), and `input.rs:149-151` sets `session_dirty` when it
returns true — good. But the flag is also set at fourteen other sites, several of
them *click* handlers in `render.rs` that only change focus
(`render.rs:710-711`, `:738-739` call `focus_main_tile`; `:796-798`, `:819-821`
call `focus_dock_tile`). Meanwhile `Workspaces::switch` — which changes `active`,
a persisted field (`session.rs:236-239`) — is reached through
`apply_workspace_action`'s `workspace::switch_N` arm (`workspaces.rs:1016-1024`)
so it *is* covered, but only by accident of the router's blanket `true`.

The per-call-site pattern means the question "does this mutation persist?" has
fourteen answers. `take_dirty_session_write` compensates by *also* diffing
serialized tiles, frame versions, and usage versions (`session_io.rs:33-42`) —
which is what actually makes the system correct, and makes most of the fourteen
flag sets redundant.

**Impact:** maintenance hazard, not a live bug: a fifteenth mutation site added
without the flag is covered only if it happens to change a diffed value.
Structural focus and dock visibility are *not* diffed.

**Direction:** diff the layout too — hash or compare `Workspaces` (it already
derives the needed equality on `Tree`/`Dock`, `tree.rs:185-191`,
`docks.rs:56-73`) — and delete the flag. Failing that, set the flag in exactly
one place: a `workspaces_mut()` accessor that every mutation goes through.

### M6. `move_divider` refuses instead of clamping, asymmetrically with drag

**Location:** `src/tiling/tree.rs:645-707` (`move_divider`),
`tree.rs:708-806` (`drag_divider`)

`move_divider` computes both new ratios and returns `false` without mutation if
either drops below `MIN_RATIO` (`tree.rs:696-700`) — "never partially applies",
which is right. `drag_divider` instead *clamps* to the limit
(`tree.rs:787-789`: `.clamp(MIN_RATIO, total - MIN_RATIO)`). So the same gesture
by keyboard stops short of the minimum while by mouse it reaches it exactly:
holding the resize key leaves a visible gap the mouse can close. The docs record
this as intentional ("Unlike discrete keyboard resize, dragging beyond the limit
clamps to it", `tree.rs:734-735`, and `tiling.md`), but the philosophy is that
keyboard is the primary interface and mouse the derivative — here the derivative
can reach a state the primary cannot.

**Impact:** a keyboard-only user cannot produce a layout a mouse user can. Small,
but it is exactly the asymmetry §2 of the charter forbids.

**Direction:** clamp in `move_divider` too, and keep the all-or-nothing rule by
computing the clamped pair first and returning `false` only when the clamp yields
no change (the `1e-6` test `drag_divider` already uses at `tree.rs:790-792`).

---

## Minor

### m1. `EPS` is an absolute tolerance on ratio-bounded geometry

**Location:** `src/tiling/tree.rs:60-62`, used in `neighbor`
(`tree.rs:544-580`)

The comment is admirably honest: "MIN_RATIO bounds ratios, not absolute size, so
deeply nested layouts can in principle produce tiles thinner than EPS whose
adjacency checks then fail; unreachable in realistic layouts (measured clean at
<= 12 tiles)". That is the right kind of comment. But the failure mode —
directional focus silently finding no neighbour — is invisible when it happens,
and `1e-3` in *unit* space is 1px at 1000px. Consider making `neighbor` take the
tolerance as a fraction of the focused tile's own extent, or assert in debug
builds when a laid-out tile is thinner than `EPS`.

### m2. `Tree::split`'s no-focus branch is unreachable-but-handled, three ways

**Location:** `src/tiling/tree.rs:399-437`

The `(Some(root), None)` arm carries a 10-line comment explaining it is
degenerate, then handles it, then has a nested `None => self.root =
Some(Node::Leaf(new))` arm commented "Unreachable (every constructible root
bottoms out in at least one leaf)". Two layers of defence against a state the
same comment says cannot exist. The never-lose-a-tile instinct is right; the
expression of it is three code paths a reader must reason about. Prefer making
the state unrepresentable — `focused: TileId` (not `Option`) whenever `root` is
`Some`, i.e. `root: Option<(Node, TileId)>` — or keep one branch and
`debug_assert!`.

### m3. Duplicated tree walks: `holds_anywhere` / `node_holds` / `find_stack` / `path_to`

**Location:** `src/tiling/tree.rs:135-169`, `tree.rs:1175-1190`

Four near-identical recursive descents, each with its own `Stack`-membership
convention: `node_holds` treats a stack as holding *any* member,
`collect_visible` (`tree.rs:171-183`) takes only `children[*active]`,
`collect_leaves` (`tree.rs:998-1011`) takes all members, and `path_to`
(`tree.rs:1175`) stops at a stack containing the target. The conventions are each
correct for their caller and individually documented, but the reader must hold
four rules. A single `visit` with an explicit `StackPolicy::{Active, All}`
parameter would make the distinction a *value* instead of a convention, and
would have made C1 harder to write.

### m4. `remove_focused` recomputes `tiles()` twice, allocating both times

**Location:** `src/tiling/tree.rs:446-495`

`let pre_close_tiles = self.tiles()` (line 458) then `let post_close_tiles =
self.tiles()` (line 487) — two `Vec<TileId>` allocations per close, plus
`find_stack` twice (lines 466, 479). A close is not a hot path, so this is
clarity more than cost: the two-phase structure obscures that the whole function
is "pick a refocus target". Consider computing the index once and deriving the
survivor from the removal itself (`remove_leaf` already knows which node it
collapsed).

### m5. `Workspaces::alloc_tile` has no exhaustion check and no wrap guard

**Location:** `src/tiling/workspaces.rs:797-803`, `session.rs:719-724`
(`tile_id_to_i64`)

`alloc_tile` does `self.next_tile += 1` with the doc noting "The counter has no
exhaustion check" — fine at u64. But `tile_id_to_i64` casts to `i64` *unchecked*
("This cast does not check the bound; a wrapped negative leaf or stack ID is
rejected on load"), so an ID above `i64::MAX` serializes as negative and the
whole session is then rejected on next load — silent total layout loss rather
than a caught error. Also `from_parts` seeds `next_tile` from
`claimed.iter().map(|id| id.0).max()` (`workspaces.rs:900`), where `claimed`
contains main-tree tiles *and* surviving dock tiles — correct, but it means a
hostile session with one huge ID permanently pins allocation near the boundary.
Clamp or reject at `alloc_tile`, where the error can still be reported.

### m6. `Predicate::eval` semantics for a missing key are surprising and only doc-guarded

**Location:** `src/keymap/context.rs:61-77`, `keymaps.md`

`Eq` and `NotEq` are both false when the key is absent
(`context.rs:66-67`: `lookup(...).is_some_and(...)`), so `mode != insert` does
*not* match a frame with no `mode` — while `!(mode == insert)` does. The doc
states this precisely ("Both `==` and `!=` are false when the key is absent;
`!(mode == insert)` therefore differs from `mode != insert`"). It is a defensible
choice, but it is the kind of thing a trader editing `keymap.toml` will get wrong
silently, with no diagnostic. Consider a warning diagnostic when a `!=`
comparison names a key no registered context ever defines — the fragment checker
(`fragments.rs:52-101`) already shows the shape of such a check.

### m7. The fragment filter rejects `!=` as collateral damage

**Location:** `src/keymap/fragments.rs:104-116` (`non_conjunction_token`)

`non_conjunction_token` searches for `"!"` anywhere, which catches the `!` in
`!=` — so a module author cannot write `context = "cvi && mode != insert"`, a
perfectly scoped conjunction. The doc admits it ("It rejects `!`, `||`, and `(`
anywhere in the text, including inside quoted values; this also rejects `!=`").
Given m6 makes `!=` the *less* surprising operator for an absent key, forbidding
it in fragments pushes module authors toward the trickier form. Scan for `!` not
followed by `=`, and for `||` / `(` as now.

### m8. `check_fragment` clones every kept entry

**Location:** `src/keymap/fragments.rs:56-102`, `fragments.rs:139-153`
(`splice`)

`kept.push(entry.clone())` per surviving entry, then `splice` clones every
`LayerDoc` in both groups (`fragments.rs:141-152`, three `.cloned()` /
`.clone()` passes). This runs on every reload (`hot_reload.rs:107-111` splices
the retained fragments each time) — a full deep clone of every keymap document
per accepted reload. Reload is not per-frame, so this is a Minor, but
`apply_reload` already avoids a per-poll `builtin.clone()` for exactly this
reason (`shell/mod.rs:1417-1421` comment: "a clone per 500ms poll would be pure
per-frame churn"). The fragments could be `Arc<LayerDoc>` and spliced by
reference.

### m9. `apply_workspace_action` dispatches on raw strings

**Location:** `src/tiling/workspaces.rs:925-1028`

A 100-line `match action.0.as_str()` over 24 string literals, ending in
`strip_prefix("workspace::switch_")` + `parse::<u8>()`. Every ID is duplicated
between here and `defaults.rs`'s `register_builtin_actions`
(`defaults.rs:110`) and `BUILTIN_KEYMAP` (`defaults.rs:24`) — three places one
typo can diverge, caught only by the unknown-action *warning* at
`build.rs:137-146`, which skips the binding silently from the user's point of
view. "Bindings are a promise" (charter §2) argues for making this
unrepresentable: a `WorkspaceAction` enum with `FromStr`, registered *from* the
enum, so an action that exists has a router arm by construction.

### m10. `stack_position` scans every workspace, per tile, per render

**Location:** `src/tiling/workspaces.rs:832-836`,
`src/shell/occupants.rs:357`

`Workspaces::stack_position` does `self.spaces.values().find_map(...)`, and each
`Workspace::stack_position` calls `region_of` (which walks the main tree then all
three docks, `workspaces.rs:483-495`) then `find_stack`. `occupants.rs:357` calls
it once per tile in `creation_order` on every render — so the cost is
O(tiles x workspaces x tree-size) per frame. The guard `if self.stack_sent.get(id)
== Some(&now) { continue; }` (`occupants.rs:358`) skips the *delivery*, not the
lookup. For ≤12 tiles this is noise; it is the wrong shape for the "8ms pure UI"
budget to rely on. Cache stack positions when the tree mutates, or compute the
whole map once per render instead of per tile.

### m11. `Dock::from_parts` heals focus; the main tree "tolerates `None`"

**Location:** `src/tiling/docks.rs:126-146`, `workspaces.rs:684-700`

`Dock::from_parts` refocuses the first tile when a non-empty dock tree lost its
focus, with a comment explaining the `Workspace` verbs lean on
"a focusable dock has a focused tile" — "where the main tree's long-standing
tolerate-`None` behavior is left as is." `Workspace::from_parts` then does the
same heal for the main tree anyway (`workspaces.rs:690-694`). So the asymmetry
the comment preserves no longer exists at the restore path; it exists only for
`Tree` used directly. Either make `Tree::from_parts` itself supply focus for a
non-empty tree (removing both call-site heals and m2's degenerate branch), or
update the comment — it currently misdescribes the code below it.

### m12. `parse_workspace` accepts a negative `focused`/`fullscreen` by clamping

**Location:** `src/session.rs:487-495`, `session.rs:634-637`

`.map(|v| TileId(v.max(0) as u64))` turns `focused = -5` into `TileId(0)`
silently, with no warning — whereas a negative *leaf* id is a hard error
(`session.rs:793-796`: "leaf id {id} is negative") and a negative stack member
likewise (`session.rs:845-848`). `TileId(0)` is never allocated (`alloc_tile`
pre-increments, `workspaces.rs:798-800`), so it dangles and `from_parts` drops it
— the outcome is right, the silence is not. Warn, for consistency with the
neighbouring readers.

### m13. `reload::scan` cannot distinguish a failed scan from a deletion

**Location:** `src/reload.rs:41-68`, `reload.rs:26-35` (`changed_since`)

`scan_dir` returns early on an unreadable directory and `continue`s past
unreadable entries/metadata/mtimes, so a transient permission error or a slow
network share reads as "every file was deleted" — which `changed_since` reports as
a change, triggering a full reload of a config the loader will then also fail to
read. The doc states it ("Scan failures are silently skipped and may look like
removals"). With keep-last-good the outcome is safe, but the user sees a
`KeptLastGood` error banner (`reload.rs:106-115`) caused by a filesystem blip
rather than their edit. Distinguish `Err` from empty: an unreadable *directory*
should leave the previous snapshot entry for that directory intact.

### m14. mtime-only change detection misses same-second edits

**Location:** `src/reload.rs:19-35`, `reload.rs:60-66`

The snapshot stores `(path, mtime)` and compares by equality, so "an edit with an
unchanged mtime does not" trigger reload (documented). On filesystems with
1-second mtime granularity, a script that writes `keymap.toml` twice within one
second — or a write in the same second as the baseline scan — is invisible until
the next unrelated change. Also the first poll establishes the baseline *without*
loading (`shell/mod.rs:1389-1405`), so an edit between startup load and that
first poll is missed entirely (also documented). Adding file length to the
snapshot key costs nothing and closes the common case.

### m15. `palette_usage` load-time pruning uses a different rule than record-time

**Location:** `src/palette_usage.rs:88-110` (`record`),
`palette_usage.rs:148-181` (`from_toml`)

`record` prunes by `(bonus(now), last_used, key)` — the time-dependent ranking
bonus. `from_toml` prunes by `Reverse((last_used, count, Reverse(key)))` — "this
load-time ordering does not use the time-dependent ranking bonus". Two rules for
one cap means a save→load cycle can drop a different entry than the in-memory
path would, so usage history is not round-trip stable at the cap. Harmless
(ranking is a heuristic) but it is a silent asymmetry; using `bonus` in both, with
`now` threaded into `from_toml`, would make the cap one rule.

### m16. `perf::IDLE_CUTOFF` is coupled to `RELOAD_POLL_INTERVAL` by value only

**Location:** `src/perf.rs:27-53`, `src/shell/hot_reload.rs:26-30`

Both are 500ms, and `perf.rs` carries a 25-line comment explaining that if they
drift apart "an idle diagnostics tile could instead pin the app in a
self-sustaining full-repaint loop: notify -> repaint -> interval recorded as a
real frame -> ... -> notify." The analysis is excellent and the risk is real; the
enforcement is a comment. `hot_reload.rs:28-30` has the matching note. Make it
structural: `const IDLE_CUTOFF: Duration = RELOAD_POLL_INTERVAL;` (or a
`const _: () = assert!(IDLE_CUTOFF >= RELOAD_POLL_INTERVAL)` like the one
`palette_usage.rs:29` already uses for `MAX_BONUS`). The codebase knows this
technique; it just did not apply it here.

### m17. Comments cite task numbers and spec sections as load-bearing context

**Location:** 244 occurrences across the non-dialog files; concentrated in
`shell/mod.rs` (99), `shell/render.rs` (41), `module.rs` (16),
`shell/status.rs` (14), `shell/occupants.rs` (12); in the pure layer:
`tiling/tree.rs:1`, `perf.rs:1,36`, `tips.rs:1`, `log_persist.rs:1-16`

CLAUDE.md is explicit: "A code comment should state the local invariant and
failure it prevents; it should not require a task number or spec section to make
sense." Many of these comments *do* also state the invariant — `perf.rs:36-53`
explains the whole coupling before citing "Phase 4b final review, MAJ-4" — so the
citation is additive. But others are citation-first: `occupants.rs:200-210`
("Phase 4b Task 5 fix round 1, MAJ-2"), `render.rs`'s "post-merge review cleanup
8", `log_persist.rs:1-16` (a 16-line module doc that is mostly the history of
which phase moved the write path). The worst offender is `log_persist.rs`, whose
doc spends more lines on provenance than on behaviour. Strip the provenance to
the git history, keep the invariant.

### m18. `ActionRegistry::name_of_hash` is O(n) with a different collision rule than `hash_names`

**Location:** `src/actions.rs:65-80`

`name_of_hash` scans sorted keys recomputing `fnv1a` per entry and returns the
*first sorted* id on collision; `hash_names` (the shared `Arc<RwLock<HashMap>>`)
retains the *most recently registered*. The doc states both. Two answers for one
question, in the crash-reporting path where a wrong action name misleads a
post-mortem. Since `hashes` is already maintained on every `register`
(`actions.rs:43-48`), `name_of_hash` could just read it — one rule, O(1).

### m19. `config_write::writer` does a best-effort `absolute()` for map identity

**Location:** `src/config_write.rs:43-55`

`std::path::absolute(dir).unwrap_or_else(|_| dir.to_path_buf())` keys the
per-directory `Writer`. On failure the key is the *relative* path, so the same
directory reached two ways gets two `Writer`s and loses ordering between them.
The module doc is honest ("Ordering coordinates this process's writers using the
same directory path, not other processes or symlink aliases"), and every
production caller passes the configured absolute `user_dir` — so this is latent.
Worth a `debug_assert!(dir.is_absolute())` to keep it latent.

---

## Ideas

### I1. Tree invariants deserve property tests, not more examples

There are ~150 tests in `tiling/tree.rs` and ~120 in `workspaces.rs`, all
example-based, and `rg proptest|quickcheck|arbitrary` finds nothing in the
workspace. The tree has four crisp invariants that no test asserts globally:
every `Split` has `>= 2` children, `children.len() == ratios.len()`, ratios are
finite/positive/sum-to-1, every `Stack` has `>= 2` members with
`active < len`, and each `TileId` appears once per workspace. A single
`check_invariants(&Tree)` helper plus a generated random verb sequence
(split/close/move/stack/unstack/fullscreen/resize/drop) would cover the whole
surface that 270 hand-written cases sample. This is the highest-value test
investment in the crate and would likely have caught C1 and M4. It also
directly serves the mutation-harness discipline in CLAUDE.md: an invariant
assertion is exactly what a mutation cannot survive.

### I2. TODO.md "Shared key bindings" — the layering already supports it

`config_write` writes only the user layer by design (`config_write.rs:105-118`
`doc_path` refuses every other layer), and the keymap compiler already consumes
desk documents in precedence order (`build.rs:60-166`). So "shared key bindings"
is not a new mechanism — it is `$GEODE_DESK_CONFIG/keymap.toml`, which works
today. What is missing is *discoverability*: the keybindings dialog shows
overrides (`keybindings_view.rs:110`) but the TODO's "Diff between [pres] and
[builtin] badge?" suggests provenance is not visible enough. `Binding::layer`
(`build.rs:22`) already carries it; surface it as a per-row chip.

### I3. TODO.md "Improve key helper to show hints when holding down mod key"

`whichkey.rs` (394 lines) already renders pending-sequence hints, and
`Matcher::pending()` (`matcher.rs:95-97`) exposes the state. A mod-key-held hint
needs a *modifier-change* event rather than a keystroke, which the matcher has no
concept of — and deliberately so (it is pure). The clean shape: `ShellView`
observes modifier changes and asks the keymap for all bindings whose first
keystroke carries those modifiers and whose predicate passes the current stack.
That is a new pure query on `Keymap` (`bindings_starting_with(mods, stack)`),
which would *also* give M2's indexing a second customer.

### I4. TODO.md "Allow dialogs on top of dialogs (a stack)" interacts with the matcher

`handle_key_down` gates on `self.modal.is_some() || window.has_active_dialog(cx)`
as a boolean (`input.rs:568`), and `dialog.rs:162` calls `matcher.cancel()` on
open. A dialog *stack* means the modal field becomes a `Vec` and the cancel
semantics need deciding: does pushing a second dialog cancel the first's pending
sequence? Since `Matcher` has no timeout by design (`matcher.rs:25-27`: "There is
no timeout in this engine; callers cancel pending input explicitly"), the stack
must own that decision explicitly. Worth settling before the feature, not during.

### I5. TODO.md "Separate windows" — `Workspaces` is already per-window-shaped

`Workspaces` owns its own tile allocator (`workspaces.rs:733`, shared across that
collection's trees and docks) and `ShellView` is documented as "the retained GPUI
entity for **one window**" (`shell.md` §State ownership). So a second window is a
second `Workspaces` — but then `TileId`s collide across windows, and
`config_write`'s directory-scoped FIFO (`config_write.rs:28-41`) becomes the
cross-window serialization point it was designed to be. The session format,
however, has one `active` and one `workspaces` table (`session.rs:230-245`) with
no window dimension: that is the piece that needs designing first.

---

## Systemic patterns

**The good pattern, applied unevenly.** This codebase has a real technique for
making invariants structural: `const _: () = assert!(...)` in
`palette_usage.rs:29`, the `Delivery` enum's deliberate non-wildcard match
(`module.rs:187+` and its doc: "refuses to compile the moment a new variant
lands"), `doc_path`'s layer guard, `Dock::set_size` owning the one clamp. Where
the technique is applied, the code is excellent. Where it is not — C1's
fullscreen invariant patched at three call sites, m16's coupling held by comment,
M5's dirty flag set at fourteen sites, m9's stringly-typed router — the same
codebase relies on prose. Nearly every Major here is "a rule that exists, stated
in a comment instead of the type system."

**Documentation as a substitute for fixing.** `docs/current/tiling.md` and
`shell.md` are unusually precise, and several of the findings above are *already
documented as known limitations*: the quit-hook save race (M1), the keyboard/mouse
resize asymmetry (M6), the missing duplicate-ID check (M4), scan failures looking
like removals (m13), mtime granularity (m14). Documenting a sharp edge is much
better than hiding it, and this is a deliberate, defensible house style. But for
a handful of these the documented behaviour is a *defect* rather than a boundary
— losing the last layout save and painting a hidden tile fullscreen are not
trade-offs a user would choose. Worth a pass asking, of each documented
limitation: "is this a boundary, or a bug we wrote down?"

**Comment-to-code ratio.** Several files are more comment than code —
`tree.rs:645-680` is 35 lines of doc for a 60-line function; `perf.rs:27-53` is
25 lines for one constant; `occupants.rs`, `render.rs`, and `mod.rs` carry long
review-provenance blocks. The *content* is usually genuinely load-bearing
(`move_divider`'s doc explains a non-obvious vim-model decision that a reader
would otherwise get wrong). But the provenance half (m17) is pure cost, and the
volume makes the load-bearing half harder to find. Separating "why this rule"
(keep, in the doc comment) from "which review asked for it" (drop, it is in git)
would cut these files meaningfully without losing anything.

**Purity held, genuinely.** No gpui import anywhere in `tiling/` or `keymap/`;
`perf` takes caller-measured durations rather than reading a clock; `reload`
splits `scan` (I/O) from `decide` (pure); `session` splits `to_string_pretty`
(pure) from `write_atomic` (I/O). `config_write` is the only module in scope that
touches both, and it is explicitly the door. This boundary is the crate's best
architectural feature and it is not drifting.

---

## What is done well

- **The pure/gpui split is real and load-bearing.** `tiling` and `keymap` are
  fully testable without a window, and the test counts show the payoff: ~270
  tiling tests, ~40 keymap tests, all fast unit tests. The `README.md` table of
  pure cores vs gpui surfaces matches the code exactly.
- **`config_write` is the right answer to the ordered-persist problem.** The
  directory-scoped FIFO with a detached drain (`config_write.rs:66-108`) means a
  dropped result task cannot cancel an accepted write, and the transaction lock
  covers read-through-rename so a parse failure leaves the file byte-identical.
  The tests prove all three: `queued_edits_keep_submission_order_even_when_results_are_dropped`,
  `a_failed_or_panicking_save_does_not_strand_later_writes`,
  `concurrent_edits_read_after_the_previous_commit`. The project-memory
  "unordered-persist race across keyed persists" is genuinely fixed here for
  config; only session writes still bypass it (M1).
- **`catch_unwind` around each queued job** (`config_write.rs:72-76`) so a
  panicking mutation cannot strand later writes, with the panic resumed on the
  waiter. That is the correct and non-obvious choice.
- **Session recovery is thorough and local.** Every failure mode has a decided
  scope: a bad main tree rejects the session, a bad dock drops one dock's tree, a
  bad tile record drops one record, a bad frame field warns and keeps the rest.
  The recovery table in `shell.md` matches `from_toml`'s actual behaviour
  line-for-line, and ~60 session tests cover legacy encodings, hostile shapes,
  and NaN ratios.
- **Honest comments about what a mechanism does *not* guarantee.**
  `DividerAddress`'s doc ("This is not a node identity or generation. A
  structural edit can leave a valid address pointing at a different boundary")
  and `effective_binding`'s ("This is a display approximation without a live
  context stack") prevent exactly the misuse a reader would otherwise attempt.
  This is the single most valuable comment style in the crate.
- **`perf::FrameHistogram` obeys the discipline it measures:** fixed array,
  integer binary search, saturating counters, no allocation, no clock read, and
  a test pinning the bucket-boundary formula. Percentile queries are explicitly
  confined to the overlay path.
- **`Matcher`'s vim semantics are correct and tested to the edge case** —
  exact-match-beats-longer-prefix, dead-end-clears-without-retry, leading-zero-is-a-motion,
  count-survives-a-pending-sequence-but-dies-on-a-dead-end, innermost-context-decides-counts.
  The count handling in particular is the kind of thing usually gotten wrong.
- **`keymap_edit` preserves the file's own spellings** (`Binding::key_source`,
  `context_source`) because a removal is `keys.remove(spelling)` on that exact
  document — with a test naming the failure it prevents
  (`the_override_carries_the_files_own_key_spelling`: rendering `mod+h` as
  `alt+h` would miss). Correct for the right, stated reason.
- **Drop and transfer verbs check both endpoints before removing anything**
  (`workspaces.rs:521-577`), with a documented never-lose-a-tile fallback and
  `debug_assert!` on the pre-broken invariant. The comment "refusal must never
  strand the dragged tile outside every tree" names the actual stake.
