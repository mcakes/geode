# gpui-kit Upgrade (Phase 1) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Move Geode from the git-pinned gpui-component 0e2fb7a + unpinned zed gpui to the crates.io releases gpui-component 0.6.2 / gpui-kit-assets 0.6.2 / gpui-pre 0.3.5, rewrite the dependency invariant that no longer holds, swap the vendored skills to gpui-kit's, and re-point every code comment that cites the old pinned checkout.

**Architecture:** The dependency swap is four files (root `Cargo.toml`, `geode-app/Cargo.toml`, `main.rs`, `Cargo.lock`) and was proven on `probe/gpui-kit` at 0.6.0/0.3.4 with zero source errors; this plan redoes it on a fresh branch from today's `main` rather than rebasing the nine-day-old probe. Everything after that is verification and documentation: the suite, the invariant text, the skills, and 63 comment sites across 28 files that name "the pinned checkout" — each of which is re-read against the 0.6.2 / 0.3.5 source before its citation is rewritten, so a re-pointed citation never vouches for a file nobody checked.

**Tech Stack:** Rust 1.96 (upstream floor 1.90), cargo registry crates `gpui-pre` 0.3.5 (zed snapshot `d89e9c2`), `gpui-pre-platform` 0.3.5, `gpui-component` 0.6.2, `gpui-kit-assets` 0.6.2, `gpui-base` 0.6.2, `gpui-component-macros` 0.6.2 (the last two pinned directly because gpui-component names them with a caret); the `skills` CLI (`npx skills`, 1.7.0).

**Spec:** `docs/superpowers/specs/2026-09-17-geode-gpui-kit-upgrade-and-adoption-design.md` — §2 (versions and the pin rule) and §3 (Phase 1). §4 and §5 are later phases and are NOT in this plan.

## Global Constraints

- Versions are exact, copied from spec §2: `gpui = { package = "gpui-pre", version = "=0.3.5" }`, `gpui_platform = { package = "gpui-pre-platform", version = "=0.3.5", features = ["font-kit", "runtime_shaders"] }`, `gpui-component = "=0.6.2"`, `gpui-kit-assets = "=0.6.2"`, `gpui-base = "=0.6.2"`, `gpui-component-macros = "=0.6.2"`. No caret anywhere in these six. Geode never depends on the `gpui-kit` umbrella crate (spec §3.1: the alias `gpui = { package = "gpui-pre" }` keeps `#[gpui::test]` and every macro path unchanged).
- The four CI checks plus the `test-support` check must stay green on macOS (CLAUDE.md "Commands"): `cargo fmt --check`; `cargo clippy --workspace --all-targets -- -D warnings`; `cargo test --workspace`; `cargo bench --workspace --no-run`; `cargo check -p geode-shell --features test-support --all-targets`. Windows must keep building; this checkout has **no git remote**, so Windows CI is a hand-off item (Task 9), not a step.
- `zsh scripts/mutation-check.sh --anchors-only` must exit 0 before merge (CLAUDE.md). No harness entry is added by this plan: it changes no Geode behaviour.
- Spec §3.4: a change in any listed pinned-rev behaviour is a **finding to report, not a test to relax**. No test is edited, weakened or `#[ignore]`d anywhere in this plan. If a test fails after the swap, stop the task and report the failure verbatim.
- Spec §3.3 (user ruling 2026-09-17): every comment citing the pinned checkout is re-pointed to the registry path with the version in it, and the claim it rests on is re-read at 0.6.2 / 0.3.5 first. A claim that no longer holds is reported (file:line, what changed), and the comment is left untouched for the orchestrator to rule on. A re-pointed citation must not vouch for a file that changed.
- Citation spelling (one rule, every site): `crates/ui/src/<path>` → `gpui-component-0.6.2/src/<path>`; `crates/gpui/src/<path>` → `gpui-pre-0.3.5/src/<path>`; a `:NNN` or `:NNN-MMM` line suffix is dropped and the named item (`fn`, `impl`, `static`, `struct`) is cited instead, because line numbers rot on every bump; "pinned checkout" / "vendored checkout" → "pinned release"; "pinned rev" / "pinned gpui-component rev" / "pinned gpui rev" stay as written (CLAUDE.md's glossary, Task 4, defines the term as the `=`-pinned registry versions). The registry source lives at `~/.cargo/registry/src/*/gpui-component-0.6.2/src/`, `~/.cargo/registry/src/*/gpui-base-0.6.2/src/`, `~/.cargo/registry/src/*/gpui-pre-0.3.5/src/` after Task 1's first build; until then, the same tarballs are unpacked in the scratchpad (`/private/tmp/claude-501/-Users-mch-Repos-geode/36fedef0-6140-467e-8537-2c71041dac2b/scratchpad/`).
- Commits are small and per task; every commit message ends with `Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>`. Never commit `TODO.md` or `docs/modules.md` (the user's untracked notes on `main`).
- Work happens on a worktree branch off `main` (`superpowers:using-git-worktrees` at execution time), named `worktree-gpui-kit`. Do not branch from or rebase onto `probe/gpui-kit`.

---

## File Structure

| File | Responsibility in this plan |
|---|---|
| `Cargo.toml` (root) | the six `[workspace.dependencies]` entries and the comment that states the pin rule |
| `Cargo.lock` | regenerated by cargo on the first build after Task 1; committed |
| `crates/geode-app/Cargo.toml` | the assets crate's new name |
| `crates/geode-app/src/main.rs` | the one `Assets` path; one comment site (Task 7) |
| `CLAUDE.md` | the invariant bullet (Task 4), the glossary sentence (Task 4), the "gpui skills" section (Task 3) |
| `skills-lock.json`, `.agents/skills/**`, `.claude/skills/*` | the vendored skills (Task 3) |
| 28 source files listed in Tasks 5–7 | comment re-pointing; no code changes |
| `docs/superpowers/specs/2026-09-17-geode-gpui-kit-upgrade-and-adoption-design.md` | §3.4 gains an "as verified" paragraph recording the outcome (Task 8) |

No new files. No test files change.

---

### Task 1: Dependency swap and first build

**Files:**
- Modify: `Cargo.toml:35-52` (the gpui comment block; four entries become six)
- Modify: `crates/geode-app/Cargo.toml:23`
- Modify: `crates/geode-app/src/main.rs:92`
- Regenerated: `Cargo.lock`

**Interfaces:**
- Consumes: nothing.
- Produces: a workspace that builds against the registry crates; the registry source unpacked under `~/.cargo/registry/src/*/` for Tasks 5–7 to read.

- [ ] **Step 1: Record the baseline**

Run: `cd <worktree> && cargo test --workspace 2>&1 | grep -E "^test result" | awk '{p+=$4; f+=$6} END {print "passed="p" failed="f}'`
Expected: `failed=0` and a `passed=` count. Write that number down; Task 2 compares against it. (On 2026-09-08 the probe recorded 1,548; `main` has grown since.)

- [ ] **Step 2: Confirm the old resolution, so the change is visible**

Run: `cargo tree -p geode-app -e normal 2>/dev/null | grep -E "gpui(-component)? v" | sort -u`
Expected: lines ending in `(https://github.com/zed-industries/zed…)` and `(https://github.com/longbridge/gpui-component?rev=0e2fb7a…)`.

- [ ] **Step 3: Replace the dependency block in the root `Cargo.toml`**

Replace lines 35–52 (from `# gpui-component's own Cargo.toml depends on` through the `gpui-component-assets = { git = …` line) with exactly:

```toml
# GPUI and gpui-kit come from crates.io and every one of them is `=`-pinned
# (spec: docs/superpowers/specs/2026-09-17-geode-gpui-kit-upgrade-and-adoption-design.md §2).
#
# gpui-kit publishes weekly snapshots of zed's gpui crates as `gpui-pre`,
# `gpui-pre-platform` and `gpui-pre-macros` (each crate's description on
# crates.io names the zed rev it snapshots — 0.3.5 is zed@d89e9c2). gpui-kit
# itself depends on `gpui-pre` with a CARET, so an unpinned entry here would
# let a plain `cargo update` move gpui under us to a snapshot nobody has
# read. The `=` pins make this manifest and the committed Cargo.lock agree;
# a bump is a deliberate change on a branch, both families together.
#
# There is no git dependency on zed or on gpui-kit any more, so the old
# two-copies hazard (a git dep keyed on URL+reference, which a `rev` pin on
# our side used to split from gpui-component's unpinned copy) cannot recur:
# we and gpui-component both name the same registry crate.
#
# `gpui-pre-macros` rewrites `gpui::` paths inside macro output to
# `gpui_kit::` only when the calling crate depends on the `gpui-kit`
# umbrella. Geode never does — it aliases `gpui = { package = "gpui-pre" }`
# — so `#[gpui::test]` and every other macro path is unchanged.
gpui = { package = "gpui-pre", version = "=0.3.5" }
# runtime_shaders: compile Metal shaders at runtime; builds without full Xcode. No-op on non-macOS.
gpui_platform = { package = "gpui-pre-platform", version = "=0.3.5", features = ["font-kit", "runtime_shaders"] }
gpui-component = "=0.6.2"
# Renamed upstream from `gpui-component-assets` with the gpui-kit rebrand.
gpui-kit-assets = "=0.6.2"
# gpui-component names these two siblings with a CARET (`gpui-base =
# "0.6.2"`), so without a direct `=` pin on our side a `cargo update` can
# float them to a newer release than the gpui-component they were built
# with — which is exactly what happened on 2026-09-18 (a 0.6.1 component
# over a 0.6.2 base failed to compile inside gpui-component itself). They
# are depended on by `geode-app` for that reason alone.
gpui-base = "=0.6.2"
gpui-component-macros = "=0.6.2"
```

- [ ] **Step 4: Rename the assets dependency in `geode-app`**

In `crates/geode-app/Cargo.toml`, change line 23 from
`gpui-component-assets.workspace = true`
to
```toml
gpui-kit-assets.workspace = true
# Sibling pins — see the root Cargo.toml's dependency comment: gpui-component
# names these with a caret, and a direct `=` here is what keeps the family
# in step under `cargo update`. Not imported by this crate.
gpui-base.workspace = true
gpui-component-macros.workspace = true
```

- [ ] **Step 5: Rename the assets path in `main.rs`**

In `crates/geode-app/src/main.rs` line 92, change
`.with_assets(gpui_component_assets::Assets)`
to
`.with_assets(gpui_kit_assets::Assets)`

- [ ] **Step 6: Build everything, letting cargo regenerate the lock**

Run: `cargo check --workspace --all-targets 2>&1 | tail -20`
Expected: `Finished` with no errors. If cargo reports it cannot resolve `=0.3.5` against gpui-component's `gpui-pre = "0.3.1"` requirement, stop and report — the spec's premise (a caret upstream) would be wrong. If any Geode source fails to compile, stop and report the error verbatim: the probe compiled with zero source errors at 0.6.0/0.3.4 and the 0.3.4→0.3.5 diff removed only `ImageCacheItem`, `spawn_dedicated` and `ImageLoadingTask`, none of which Geode uses, so a source error is a finding.

- [ ] **Step 7: Prove there is one copy of gpui and it is the pinned one**

Run: `cargo tree --workspace -d 2>/dev/null | grep -iE "^gpui" ; echo "dupes above (expect none)"; cargo tree -p geode-app -e normal 2>/dev/null | grep -oE "gpui[a-z-]* v[0-9.]+" | sort -u`
Expected: nothing above the `dupes` line; below it exactly `gpui-base v0.6.2`, `gpui-component v0.6.2`, `gpui-component-macros v0.6.2`, `gpui-kit-assets v0.6.2`, `gpui-pre v0.3.5`, `gpui-pre-macros v0.3.5`, `gpui-pre-platform v0.3.5` (and any `gpui-pre-*` platform sub-crates at 0.3.5). No `0.3.4`, no `0.6.1`, no git URL.

- [ ] **Step 8: Check the two feature-gated builds**

Run: `cargo check -p geode-shell --features test-support --all-targets && cargo check -p geode-app --features profiling`
Expected: both `Finished`. (`profiling` maps to `gpui/profiler`, which gpui-pre 0.3.5 still declares as `profiler = ["dep:hdrhistogram"]`.)

- [ ] **Step 9: Commit**

```bash
git add Cargo.toml Cargo.lock crates/geode-app/Cargo.toml crates/geode-app/src/main.rs
git commit -m "deps: gpui-component 0.6.2 / gpui-pre 0.3.5 from crates.io, =-pinned

Replaces the git-pinned gpui-component 0e2fb7a and the unpinned zed gpui
with the registry releases. gpui-kit depends on gpui-pre with a caret,
so every entry is =-pinned and the old unpinned-git invariant is retired
(spec 2026-09-17 §2, §3.2). gpui-component-assets was renamed
gpui-kit-assets upstream.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>"
```

---

### Task 2: Full verification against the baseline

**Files:**
- Modify: none expected. If clippy or a deprecation forces a change, the fix is its own commit and is reported.

**Interfaces:**
- Consumes: Task 1's build and its baseline pass count.
- Produces: a green tree at the new versions; the list of tests that changed behaviour (expected empty).

- [ ] **Step 1: Format check**

Run: `cargo fmt --check`
Expected: no output, exit 0.

- [ ] **Step 2: Clippy with warnings as errors**

Run: `cargo clippy --workspace --all-targets -- -D warnings 2>&1 | tail -30`
Expected: `Finished`, no warnings. If gpui-pre 0.3.5 deprecates something Geode calls, the warning names the replacement; adopt the replacement only if it is a same-shape rename (report it in the commit message), otherwise stop and report. Do not add `#[allow(deprecated)]`.

- [ ] **Step 3: The whole suite**

Run: `cargo test --workspace 2>&1 | grep -E "^test result|FAILED|panicked" | awk '/^test result/ {p+=$4; f+=$6} /FAILED|panicked/ {print} END {print "passed="p" failed="f}'`
Expected: `failed=0`, `passed=` ≥ Task 1 Step 1's number. Any `FAILED` line is reported verbatim with the test name; per the global constraints, no test is edited. In particular, if any test named in spec §3.4 fails — `the_editor_gives_up_focus_before_it_is_dropped`, `menu_closes_an_open_picker_with_a_blur_before_opening`, `a_presentation_change_rebuilds_the_plan_on_a_same_column_snapshot`, `the_line_numbers_global_paints_a_gutter_on_the_next_draw`, `the_menu_rows_stop_propagation_keeps_the_pickers_focus`, `a_double_click_only_moves_the_cursor`, `dirty_and_sent_cells_are_readable_on_every_bundled_theme`, `every_header_tone_is_readable_on_every_bundled_theme`, `every_bundled_theme_keeps_generated_hues_readable` — say which, because that is a pinned-rev behaviour that changed.

- [ ] **Step 4: Benches compile**

Run: `cargo bench --workspace --no-run 2>&1 | tail -5`
Expected: `Finished`.

- [ ] **Step 5: Mutation anchors**

Run: `zsh scripts/mutation-check.sh --anchors-only`
Expected: exit 0 (no stale or duplicated anchors — this plan moved no code, so any report here is a pre-existing problem on `main`; report it, do not fix it here).

- [ ] **Step 6: Run the app headless-smoke for a panic-free start**

Run: `cargo build -p geode-app && (./target/debug/geode --demo 1000 > /tmp/geode-smoke.log 2>&1 & pid=$!; sleep 20; kill $pid; grep -iE "panic|error" /tmp/geode-smoke.log | head)`
Expected: no `panic` line. (macOS ships no `timeout`; the app runs until killed. An `error` line from the theme or asset loader is a finding: gpui-kit-assets must serve the same icon set the old assets crate did.) If no display is available the window will not open; that is fine, the check is for a panic before or during window creation only.

- [ ] **Step 7: Commit (only if Step 2 changed something)**

```bash
git add -A crates
git commit -m "deps: adopt <old> -> <new> rename for gpui-pre 0.3.5

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>"
```

---

### Task 3: Swap the vendored skills to gpui-kit's

**Files:**
- Delete: `.agents/skills/gpui/**`, `.agents/skills/gpui-component/**`, `.claude/skills/gpui`, `.claude/skills/gpui-component`
- Create: `.agents/skills/gpui-kit/**`, `.agents/skills/gpui-kit-design-guides/**`, `.claude/skills/gpui-kit`, `.claude/skills/gpui-kit-design-guides` (written by the CLI)
- Modify: `skills-lock.json` (written by the CLI), `CLAUDE.md:140-142`

**Interfaces:**
- Consumes: nothing.
- Produces: the two skills the Skill tool lists from here on: `gpui-kit`, `gpui-kit-design-guides`.

- [ ] **Step 1: Remove the retired skills**

Run: `cd <worktree> && npx --yes skills remove gpui gpui-component`
Expected: the CLI reports both removed; `ls .agents/skills` shows neither; `skills-lock.json` no longer lists them.

- [ ] **Step 2: Add gpui-kit's**

Run: `npx --yes skills add longbridge/gpui-kit --all`
Expected: the CLI installs `gpui-kit` and `gpui-kit-design-guides`. Verify: `ls .agents/skills` → exactly those two; `ls -la .claude/skills` → two symlinks into `../../.agents/skills/…`; `python3 -c "import json; print(sorted(json.load(open('skills-lock.json'))['skills']))"` → `['gpui-kit', 'gpui-kit-design-guides']`; each entry's `source` is `longbridge/gpui-kit`.

- [ ] **Step 3: Confirm the skill body is the upstream one**

Run: `head -3 .agents/skills/gpui-kit/SKILL.md`
Expected: `name: gpui-kit` and a description beginning `How to build desktop applications with GPUI Kit, the Rust framework published as the gpui-kit crate`.

- [ ] **Step 4: Rewrite the CLAUDE.md skills section**

Replace the whole `## gpui skills` section (lines 140–142) with:

```markdown
## gpui skills

The `gpui-kit` and `gpui-kit-design-guides` skills (available via the Skill tool) are vendored into this repo from longbridge/gpui-kit and tracked in `skills-lock.json` (they replaced the retired `gpui` and `gpui-component` skills on 2026-09-18, with the upgrade to the crates.io releases). Use them when touching any gpui rendering, entity, async, focus, or component code. **One translation to keep in mind:** the skills are written for the `gpui-kit` umbrella crate and spell paths as `gpui_kit::component::X` / `gpui_kit::base::X`; Geode does not depend on the umbrella (see the dependency comment in the root `Cargo.toml`), so those are `gpui_component::X` and, for the unstyled layer, a re-export or `gpui_base::X` here — the item names and signatures are the same, only the prefix differs.
```

- [ ] **Step 5: Commit**

```bash
git add -A .agents .claude skills-lock.json CLAUDE.md
git commit -m "skills: gpui-kit + gpui-kit-design-guides replace gpui + gpui-component

Upstream ships the two new skills since the rebrand; the old two are
gone. CLAUDE.md notes the gpui_kit::component -> gpui_component prefix
translation, since Geode never depends on the umbrella crate.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>"
```

---

### Task 4: Rewrite the invariant in CLAUDE.md

**Files:**
- Modify: `CLAUDE.md:117` (the invariant bullet under "Workspace invariants and gotchas")

**Interfaces:**
- Consumes: Task 1's `Cargo.toml` comment (the bullet points at it).
- Produces: the glossary sentence defining "the pinned rev" that Tasks 5–7's untouched phrasing relies on.

- [ ] **Step 1: Replace the bullet**

Replace the bullet that begins `- **gpui / gpui_platform git deps must stay unpinned**` (the whole bullet, one line) with:

```markdown
- **Every gpui-kit and gpui-pre crate is `=`-pinned in the root `Cargo.toml`** — `gpui-component`, `gpui-kit-assets`, `gpui-base`, `gpui-component-macros`, `gpui-pre` (aliased `gpui`) and `gpui-pre-platform` (aliased `gpui_platform`), all from crates.io since 2026-09-18 (0.6.2 / 0.3.5; spec `docs/superpowers/specs/2026-09-17-geode-gpui-kit-upgrade-and-adoption-design.md` §2). gpui-kit depends on `gpui-pre` with a caret and gpui-component names its own siblings `gpui-base`/`gpui-component-macros` with a caret too, so an unpinned entry would let `cargo update` move gpui — or split the gpui-kit family — under us (it did, on the first build of this upgrade). Bump both families together, deliberately, on a branch; read the gpui-pre crate's description on crates.io for the zed rev it snapshots. There is no git dependency on zed or gpui-kit any more, so the old two-copies hazard the previous form of this rule guarded against cannot recur. The full explanation is in the comment in the root `Cargo.toml`. **Glossary:** wherever this file or a code comment says "the pinned rev", "the pinned release" or "the pinned gpui-component", it means these exact versions; the source a maintainer reads is the registry copy at `~/.cargo/registry/src/*/gpui-component-0.6.2/src/` (styled components), `gpui-base-0.6.2/src/` (unstyled behaviour) and `gpui-pre-0.3.5/src/` (gpui itself), never a git checkout.
```

- [ ] **Step 2: Check nothing else in CLAUDE.md still describes the git form**

Run: `grep -nE "stay unpinned|zed-industries|0e2fb7a|gpui-component-assets|gpui_component_assets" CLAUDE.md`
Expected: no output. Any hit is a leftover to rewrite in the same commit (quote it in the task report).

- [ ] **Step 3: Commit**

```bash
git add CLAUDE.md
git commit -m "docs: the =-pin rule replaces the unpinned-git invariant

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>"
```

---

### Task 5: Re-point citations — geode-shell module headers (path citations)

These ten files cite `crates/ui/src/<file>` paths, some with line numbers, as the basis of a recorded decision. For each site: open the cited file at 0.6.2, confirm the claim in the comment still holds, then rewrite the citation per the global spelling rule. Where the claim no longer holds, leave the comment as it is and report the site.

**Files:**
- Modify (comments only): `crates/geode-shell/src/shell/dialog.rs` (sites at lines 8, 11, 201, 227, 240, 248, 470, 558–559, 651, 654, 952, 1049), `crates/geode-shell/src/palette.rs` (749, 788, 818, 826, 951, 958), `crates/geode-shell/src/fonts.rs` (9, 28, 33, 38), `crates/geode-shell/src/theme.rs` (10–11, 50, 73), `crates/geode-shell/src/shell/sidebar.rs` (7, 22–23), `crates/geode-shell/src/shell/toolbar.rs` (3–4), `crates/geode-shell/src/shell/status.rs` (16), `crates/geode-shell/src/tiling/docks.rs` (16), `crates/geode-shell/src/listfilter.rs` (20), `crates/geode-shell/src/fontsize.rs` (12)

**Interfaces:**
- Consumes: the registry source under `~/.cargo/registry/src/*/gpui-component-0.6.2/src/` and `gpui-pre-0.3.5/src/` (present after Task 1's build).
- Produces: nothing code-level; a report listing every site as `confirmed` or `changed: <what>`.

- [ ] **Step 1: List the sites and confirm the count**

Run: `grep -nE 'pinned checkout|pinned rev|pinned gpui-component|crates/ui/src|pinned component|pinned gpui|vendored checkout' crates/geode-shell/src/shell/dialog.rs crates/geode-shell/src/palette.rs crates/geode-shell/src/fonts.rs crates/geode-shell/src/theme.rs crates/geode-shell/src/shell/sidebar.rs crates/geode-shell/src/shell/toolbar.rs crates/geode-shell/src/shell/status.rs crates/geode-shell/src/tiling/docks.rs crates/geode-shell/src/listfilter.rs crates/geode-shell/src/fontsize.rs | wc -l`
Expected: 36.

- [ ] **Step 2: Verify and rewrite each claim**

The claims, with where to confirm each at 0.6.2 (`C` = `gpui-component-0.6.2/src`, `P` = `gpui-pre-0.3.5/src`):

| Site | Claim | Confirm by |
|---|---|---|
| `dialog.rs:8-11` | `Dialog` hardwires a 250 ms entrance, every `.with_animation` unconditional | `grep -n "ANIMATION_DURATION\|with_animation" C/dialog/dialog.rs` → `LazyLock … from_secs_f64(0.25)` and two unconditional `with_animation` calls. Confirmed on 2026-09-17 by the spec author; recheck anyway. |
| `dialog.rs:201`, `:1049` | `Root` (`crates/ui/src/root.rs`) renders the dialog/notification layers and holds the focused input | `grep -n "focused_input\|render_dialog_layer\|render_notification_layer" C/root.rs` |
| `dialog.rs:227` | the `Input`'s key handler in `input/state.rs` returns without propagating for the named keys | read `C/input/state.rs`'s `on_key_down`/`handle_key_down` (grep `fn .*key_down`) |
| `dialog.rs:240`, `:248` | `Window::dispatch_key_event` ordering; `KeyContext::depth_of` | `grep -n "fn dispatch_key_event" P/window.rs`; `grep -rn "fn depth_of" P/keymap/` |
| `dialog.rs:470` | `InputState::set_value` emits no `InputEvent::Change` | `grep -n "fn set_value" -A 25 C/input/state.rs` → no `cx.emit(InputEvent::Change` inside |
| `dialog.rs:558-559` | the dialog's `with_animation("slide-down", ..)` closure at `delta = 1.0` | `grep -n '"slide-down"\|fade-in' C/dialog/dialog.rs` |
| `dialog.rs:651` | the command palette builds `Input::prefix(Icon::new(IconName::Search).text_color(muted_foreground))` + `appearance(false)` | `grep -n "IconName::Search" -A 4 C/command/state.rs` |
| `dialog.rs:654`, `palette.rs:958` | `appearance(false)` guards only background and border, never the prefix child | `grep -n "appearance" C/input/input.rs` and read the render block around `.children(prefix.map(` |
| `dialog.rs:952` | the overlay colour token the dialog uses is `overlay_color` / `theme.overlay` | `grep -n "overlay" C/dialog/dialog.rs` |
| `palette.rs:749` | `StyledText::with_highlights` takes `(Range, HighlightStyle)` pairs and needs no seed `TextStyle`, unlike `with_default_highlights` | `grep -n "pub fn with_highlights\|pub fn with_default_highlights" P/elements/text.rs` — both exist with those shapes |
| `palette.rs:788` | `.appearance(false)` gates `Input`'s background, border and rounding (all three) but not `input_px`/`input_py` padding | `grep -n "appearance\|input_px\|input_py" C/input/input.rs` — the three style calls sit under the `appearance` gate, the padding does not |
| `palette.rs:818`, `:826` | scroll: `crates/ui/src/scroll/`, `list/list.rs` mechanism | `ls C/scroll/`; `grep -n "scroll_to_item\|ListState" C/list/list.rs | head` |
| `palette.rs:951` | `command/state.rs` builds the same prefix/appearance pair | same grep as `dialog.rs:651` |
| `fonts.rs:9` | "checked against the pinned checkouts named in `crates/geode-app/Cargo.toml`" | the checkouts are now named in the root `Cargo.toml`; rewrite to "checked against the pinned releases named in the root `Cargo.toml`" |
| `fonts.rs:28`, `:33`, `:38` | `Theme.font_family` default `.SystemUIFont`; `Root::render` applies `.font_family(cx.theme().font_family.clone())`; `ThemeConfig` has optional `font_family`/`mono_font_family` | `grep -n "SystemUIFont\|mono_font_family" C/theme/mod.rs C/theme/schema.rs`; `grep -n "font_family(cx.theme()" C/root.rs` |
| `theme.rs:10-11`, `:50`, `:73` | `theme/default-theme.json` exists; `ThemeConfig` in `theme/schema.rs`; `apply_config` skips the Base layer at this rev | `ls C/theme/`; `grep -n "pub struct ThemeConfig" C/theme/schema.rs`; `grep -n "fn apply_config" -A 30 C/theme/mod.rs` and confirm the Base-layer remark |
| `sidebar.rs:7`, `:22-23` | `Sidebar` is a ~255 px, 48 px-collapsed, `ListState`-virtualised animated drawer; `Avatar::small` is 24 px and the nameless fallback paints `IconName::User` in `theme.background` | `grep -n "DEFAULT_WIDTH\|COLLAPSED_WIDTH\|ListState" C/sidebar/mod.rs`; `grep -n "IconName::User\|background" C/avatar/avatar.rs` |
| `toolbar.rs:3-4` | `TitleBar` draws platform window controls and owns drag/double-click; `title_bar_options()` exists | `grep -n "pub fn title_bar_options\|traffic_light" C/title_bar.rs` |
| `status.rs:16` | `StatusBar` at `status_bar.rs` with `left`/`right` | `grep -n "pub fn left\|pub fn right" C/status_bar.rs` |
| `docks.rs:16` | `crates/ui/src/dock/` ships `DockArea` etc. | `ls C/dock/`; `grep -n "pub struct DockArea" C/dock/*.rs` |
| `listfilter.rs:20` | `left`/`right`/`home`/`end` are swallowed by the focused `Input` | `grep -n '"left"\|"right"\|"home"\|"end"' C/input/state.rs | head` |
| `fontsize.rs:12` | the components overwhelmingly use rem-based helpers | `grep -c "rems\|text_sm\|text_base" C/button/button.rs C/input/input.rs` (non-zero) |

Rewrite each confirmed site per the spelling rule. Two worked examples, so the style is unambiguous:

`fonts.rs:33` before:
```
//!   `Root::render`
//!   (`crates/ui/src/root.rs:588`) applies `.font_family(cx.theme()
```
after:
```
//!   `Root::render`
//!   (`gpui-component-0.6.2/src/root.rs`, the root `div`'s `.font_family(cx.theme()
```
(the line number is dropped; the item — the root `div` — is named.)

`dialog.rs:651` before:
```
/// .text_color(muted_foreground))` + `appearance(false)` pair (pinned
/// checkout, `crates/ui/src/command/state.rs:838-846`) — so the icon is
```
after:
```
/// .text_color(muted_foreground))` + `appearance(false)` pair (pinned
/// release, `gpui-component-0.6.2/src/command/state.rs`, `Command`'s
/// searchable-header render) — so the icon is
```

- [ ] **Step 3: Confirm no old spelling survives in these files**

Run: `grep -nE 'crates/ui/src|crates/gpui/src|pinned checkout|vendored checkout' crates/geode-shell/src/shell/dialog.rs crates/geode-shell/src/palette.rs crates/geode-shell/src/fonts.rs crates/geode-shell/src/theme.rs crates/geode-shell/src/shell/sidebar.rs crates/geode-shell/src/shell/toolbar.rs crates/geode-shell/src/shell/status.rs crates/geode-shell/src/tiling/docks.rs crates/geode-shell/src/listfilter.rs crates/geode-shell/src/fontsize.rs`
Expected: no output — unless a site was reported as `changed` and deliberately left, in which case exactly those lines.

- [ ] **Step 4: Build and format (comments can still break a doc test or rustfmt's width)**

Run: `cargo fmt --check && cargo test -p geode-shell --doc 2>&1 | tail -3`
Expected: fmt clean; doc tests `ok`.

- [ ] **Step 5: Commit**

```bash
git add crates/geode-shell
git commit -m "docs(shell): re-point pinned-checkout citations to the 0.6.2 / 0.3.5 registry sources

Every claim re-read at the new versions before its citation moved;
line numbers replaced by item names.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>"
```

---

### Task 6: Re-point citations — geode-shell behavioural claims

These sites say "at the pinned rev" about a gpui or gpui-component behaviour rather than citing a path. The phrase stays (Task 4's glossary defines it); the job is to re-verify each claim at 0.3.5 / 0.6.2 and rewrite only the sites that also name a checkout or a `crates/…` path.

**Files:**
- Modify (comments only, where a path or "checkout" appears): `crates/geode-shell/Cargo.toml` (15), `crates/geode-shell/src/module.rs` (514), `crates/geode-shell/src/shell/render.rs` (105), `crates/geode-shell/src/shell/occupants.rs` (318), `crates/geode-shell/src/shell/palette_ctl.rs` (24), `crates/geode-shell/src/shell/hot_reload.rs` (167), `crates/geode-shell/src/shell/mod.rs` (550, 567, 1130), `crates/geode-shell/src/shell/profiling_hook.rs` (5), `crates/geode-shell/src/shell/input.rs` (712), `crates/geode-shell/src/shell/commandline_ctl.rs` (258), `crates/geode-shell/src/shell/tests/drag.rs` (17, 1503), `crates/geode-shell/src/shell/tests/mod.rs` (547), `crates/geode-shell/src/shell/tests/palette.rs` (634)

**Interfaces:**
- Consumes: the registry sources.
- Produces: the report of `confirmed` / `changed` per site.

- [ ] **Step 1: Verify each claim**

| Site | Claim | Confirm by (`C`/`P` as in Task 5) |
|---|---|---|
| `Cargo.toml:15` | gpui's `profiler` feature uses its own overlay, no Tracy client | `grep -n "^profiler\|tracy" P/../Cargo.toml` (the crate's manifest: `profiler = ["dep:hdrhistogram"]`, no `tracy`) |
| `module.rs:514` | dropping an `InputState` does not blur: `Root` registers the focused input as a strong `AnyInputState` via `input::state::sync_focused_input_registry` and only unregisters it from that input's own render | `grep -rn "sync_focused_input_registry\|focused_input" C/input/state.rs C/root.rs` — the registry fn still exists and the unregister call sits inside `Input`'s render path |
| `render.rs:105` | `FocusHandle::for_id` refuses a zero-refcount entry, so `Window::focused` reports `None` only for a DROPPED handle | `grep -n "fn for_id" -A 12 P/window.rs` — the refcount check is still there |
| `occupants.rs:318` | `Window::focused_node_id` falls back to `root_node_id` when the focused id is absent from the rendered tree | `grep -n "fn focused_node_id" -A 12 P/window.rs` |
| `palette_ctl.rs:24` | `InputState::set_value` emits no `Change` | same as Task 5 `dialog.rs:470` |
| `hot_reload.rs:167`, `tests/mod.rs:547` | the test executor never advances its simulated clock on `run_until_parked` — `TestScheduler::run` is a plain `while step() {}` | `grep -rn "fn run_until_parked" P/ ; grep -rn "struct TestScheduler" -l P/` then read its `run` (if `TestScheduler` was renamed in the 0.3.5 snapshot, cite the new name and say so in the report) |
| `mod.rs:550`, `input.rs:712` | `Window::dispatch_key_event` / `dispatch_action_on_node` routing; a consumed keystroke still reaches every `on_key_down` listener via `finish_dispatch_key_event` / `dispatch_key_down_up_event` | `grep -n "fn dispatch_key_event\|fn dispatch_action_on_node\|fn finish_dispatch_key_event\|fn dispatch_key_down_up_event" P/window.rs` — all four exist; read the first for the order |
| `mod.rs:567` | up/down have a `KeyBinding` in the `Input` context but the element attaches their `on_action` only `.when(self.is_multi_line…)` | `grep -n '"up"\|"down"' C/input/state.rs \| head; grep -n "is_multi_line\|multi_line()" C/input/input.rs \| head` |
| `mod.rs:1130` | `cx.observe_window_activation` exists and is fed from the platform's `on_active_status_change` | `grep -rn "fn observe_window_activation" P/; grep -rn "on_active_status_change" P/platform/ \| head -3` |
| `profiling_hook.rs:5` | "Findings at the pinned gpui rev (zed e3adf43)" | the rev is now zed@d89e9c2 via gpui-pre 0.3.5; re-verify each finding in the paragraph against `P/`; rewrite the parenthetical to "(gpui-pre 0.3.5, zed@d89e9c2; first recorded at zed e3adf43)" |
| `commandline_ctl.rs:258` | `InputState::set_value` emits no `InputEvent::Change` | same as Task 5 `dialog.rs:470` |
| `tests/drag.rs:17`, `:1503` | `TestPlatform` records `set_cursor_style` into a private field with no accessor; `Window::dispatch_key_event` draws first whenever the window is dirty | `grep -rn "fn set_cursor_style" -A 4 P/platform/test/platform.rs` (no `pub fn` reader for the field); `grep -n "fn dispatch_key_event" -A 15 P/window.rs` (the `draw` call precedes dispatch) |
| `tests/palette.rs:634` | neither of the `Input`'s `ctrl+a` handlers (`SelectAll`, and the reclaimed one) calls `cx.propagate()` | `grep -n "fn select_all" -A 10 C/input/state.rs` — no `propagate` |

- [ ] **Step 2: Rewrite the sites that name a checkout or path**

Apply the spelling rule (`pinned checkout`/`vendored checkout` → `pinned release`; any `crates/…` path → registry path; `zed e3adf43` per the `profiling_hook.rs` row). Sites that only say "pinned rev" with no path are left byte-identical once confirmed.

- [ ] **Step 3: Check, fmt, commit**

Run: `grep -nE 'crates/ui/src|crates/gpui/src|pinned checkout|vendored checkout' crates/geode-shell/Cargo.toml crates/geode-shell/src/module.rs crates/geode-shell/src/shell/render.rs crates/geode-shell/src/shell/occupants.rs crates/geode-shell/src/shell/palette_ctl.rs crates/geode-shell/src/shell/hot_reload.rs crates/geode-shell/src/shell/mod.rs crates/geode-shell/src/shell/profiling_hook.rs crates/geode-shell/src/shell/input.rs crates/geode-shell/src/shell/commandline_ctl.rs crates/geode-shell/src/shell/tests/*.rs; cargo fmt --check`
Expected: no grep output (bar deliberately reported sites); fmt clean.

```bash
git add crates/geode-shell
git commit -m "docs(shell): pinned-rev behavioural claims re-verified at gpui-pre 0.3.5 / gpui-component 0.6.2

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>"
```

---

### Task 7: Re-point citations — blotter, market-data and app

**Files:**
- Modify (comments only): `crates/geode-blotter/src/delegate.rs` (80, 457, 1179), `crates/geode-marketdata/src/popup.rs` (61), `crates/geode-marketdata/src/delegate.rs` (292, 347), `crates/geode-marketdata/src/tile.rs` (969, 1569, 4505), `crates/geode-app/src/main.rs` (233, 303)

**Interfaces:**
- Consumes: the registry sources.
- Produces: the report of `confirmed` / `changed` per site.

- [ ] **Step 1: Verify each claim**

| Site | Claim | Confirm by |
|---|---|---|
| `blotter/delegate.rs:80`, `:457`, `:1179` | `TableState::update_visible_range_if_need` only records a new visible range when `visible_range.len() > 1` | `grep -n "fn update_visible_range_if_need" -A 12 C/table/state.rs` — confirm the `<= 1 { return; }` guard is still there |
| `marketdata/popup.rs:61` | `SharedString` has no inline small-string form at this rev | `grep -n "pub struct SharedString\|enum SharedString" -A 5 P/shared_string.rs` — still a `String`/`&'static str` wrapper with no inline variant |
| `marketdata/delegate.rs:292`, `:347` | `warning_foreground` falls back to `primary_foreground`; `muted_foreground` over `muted` is faint | `grep -n "warning_foreground" C/theme/mod.rs C/theme/schema.rs C/theme/color.rs` — the fallback assignment |
| `marketdata/tile.rs:969` | `TableState` caches each `column()` in `col_groups`; `refresh` re-prepares | `grep -n "col_groups\|pub fn refresh" C/table/state.rs \| head` |
| `marketdata/tile.rs:1569`, `:4505` | `blur` then drop is required; `Root` holds the focused input strongly and unregisters only from that input's render | `grep -n "focused_input" C/root.rs C/input/input.rs C/input/state.rs` — the strong `Entity<InputState>` on `Root` and the unregister site inside `Input`'s render |
| `app/main.rs:233` | `App::on_app_quit` exists | `grep -n "fn on_app_quit" P/app.rs` |
| `app/main.rs:303` | the `window_title` example at the pinned checkout is the reference for `TitleBar` content | the crates.io tarball ships no `examples/`; rewrite to cite `TitleBar`'s own doc comment (`gpui-component-0.6.2/src/title_bar.rs`) and keep the example's name as "upstream's `examples/window_title`" |

- [ ] **Step 2: Rewrite per the spelling rule; confirm; commit**

Run: `grep -nE 'crates/ui/src|crates/gpui/src|pinned checkout|vendored checkout' crates/geode-blotter/src/delegate.rs crates/geode-marketdata/src/popup.rs crates/geode-marketdata/src/delegate.rs crates/geode-marketdata/src/tile.rs crates/geode-app/src/main.rs; cargo fmt --check`
Expected: no grep output (bar reported sites); fmt clean.

```bash
git add crates/geode-blotter crates/geode-marketdata crates/geode-app
git commit -m "docs(modules,app): re-point pinned-checkout citations to the registry sources

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>"
```

---

### Task 8: Whole-tree citation sweep and the spec's as-verified note

**Files:**
- Modify: `docs/superpowers/specs/2026-09-17-geode-gpui-kit-upgrade-and-adoption-design.md` (§3.4, append one paragraph)

**Interfaces:**
- Consumes: Tasks 5–7's reports.
- Produces: the spec's record of what was verified.

- [ ] **Step 1: Sweep the whole repo for anything the file lists missed**

Run: `grep -rnE 'crates/ui/src|crates/gpui/src|pinned checkout|vendored checkout|gpui-component-assets|gpui_component_assets|zed-industries/zed|0e2fb7a' --exclude-dir=target --exclude-dir=.git --exclude-dir=.agents . | grep -vE "^./docs/superpowers/(specs|plans)/|^./docs/perf.md|^./docs/phase-3-prerequisites.md|^./docs/ingest-cold-start-handoff.md"`
Expected: no output. Historical docs (specs, plans, perf, handoffs) are excluded on purpose — they record what was true when written. Any hit elsewhere is rewritten per the rule and included in this task's commit.

- [ ] **Step 2: Append the as-verified paragraph to spec §3.4**

After the table in §3.4, add:

```markdown
**As verified (Task 8 of the plan, <date>):** every behaviour in the table
held at gpui-component 0.6.2 / gpui-pre 0.3.5 and every named test passed
unchanged. Of the 63 comment sites that cited the pinned checkout, <n>
were re-pointed after their claim was confirmed at the new versions and
<m> were reported as changed: <list each as `file:line — what changed`, or
"none">.
```

Fill in the real date, counts and list from Tasks 5–7's reports.

- [ ] **Step 3: Full verification once more**

Run: `cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings 2>&1 | tail -3 && cargo test --workspace 2>&1 | grep -E "^test result" | awk '{p+=$4; f+=$6} END {print "passed="p" failed="f}' && cargo bench --workspace --no-run 2>&1 | tail -1 && cargo check -p geode-shell --features test-support --all-targets 2>&1 | tail -1 && zsh scripts/mutation-check.sh --anchors-only`
Expected: fmt clean; clippy `Finished`; `failed=0`; bench `Finished`; check `Finished`; anchors exit 0.

- [ ] **Step 4: Commit**

```bash
git add docs/superpowers/specs/2026-09-17-geode-gpui-kit-upgrade-and-adoption-design.md
git commit -m "docs: gpui-kit spec §3.4 — as verified at 0.6.2 / 0.3.5

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>"
```

---

### Task 9: Merge, cleanup and hand-off (orchestrator, not a subagent)

**Files:**
- Delete (git): branch `probe/gpui-kit`
- Modify (outside the repo, the orchestrator's own memory — a subagent must not touch these): `~/.claude/projects/-Users-mch-Repos-geode/memory/gpui-component-inventory-check.md`, `gpui-kit-migration.md`, `working-rhythm.md`

- [ ] **Step 1: Review gate**

Per the working rhythm, an independent reviewer reads the whole branch diff against the spec (§2, §3) before merge — the comment re-pointing is the part most likely to have vouched for a changed file, so the reviewer spot-checks at least five re-pointed citations against the registry source.

- [ ] **Step 2: Merge to `main`**

Run: `cd /Users/mch/Repos/geode && git status --short` — expect only the user's untracked `TODO.md` and `docs/modules.md`; if `main` carries the user's uncommitted edits, verify his tree is green, commit them as his work, then merge preserving both sides. Then `git merge --no-ff worktree-gpui-kit`, run the full verification from Task 8 Step 3 on `main`, and remove the worktree.

- [ ] **Step 3: Delete the probe branch**

Run: `git branch -D probe/gpui-kit`
Expected: `Deleted branch probe/gpui-kit`.

- [ ] **Step 4: Update the orchestrator's memories**

`gpui-component-inventory-check.md`: the inventory to list is now `~/.cargo/registry/src/*/gpui-component-0.6.2/src/` (and `gpui-base-0.6.2/src/`), not a git checkout. `gpui-kit-migration.md`: status → merged, date, commit; the "git-only" remark about headless testing corrected (`gpui_base::test_support` is released; only the `gpui_kit::test` facade is git-only). `working-rhythm.md`: the reviewer's hand-traces are against the registry sources, not `~/.cargo/git/checkouts/`.

- [ ] **Step 5: Hand the display checks to the user**

Spec §3.5's list, verbatim, plus the Windows CI item (no remote here — the user pushes): the shell opens with working traffic lights and drag; a blotter and a CVI panel paint, sort, expand and edit a cell, and `ctrl+k` opens the palette after `escape` from a cell editor; a modal opens instantly; the 38 themes switch and named-colour swatches paint; the scope-bar field types and `mod+/` returns focus to it after an overlay. Expected result on every one: no visible difference.
