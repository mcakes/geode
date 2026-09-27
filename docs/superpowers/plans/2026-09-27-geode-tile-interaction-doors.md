# geode-tile Interaction Doors Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a `geode-tile` crate holding four interaction doors (`popover`, `menu`, `confirm`, `notice`) and move the pricer, market-data, timeseries and blotter onto them, so a tile mechanism is written once.

**Architecture:** `geode-tile` sits between `geode-shell` and the feature modules (`geode-core` → `geode-shell` → `geode-tile` → modules). Each door is a pure model plus one GPUI painter; modules keep their content, their key bindings, their action ids and their precedence between notice slots, and hand the door what it paints. Migration is one module per task, each leaving the workspace green.

**Tech Stack:** Rust 2024, GPUI (`gpui-pre =0.3.5`, aliased `gpui`), gpui-component `=0.6.2`, `geode-shell` paint doors (`kbd`, `control`, `scale`, `chip`, `listrow`, `colours`) and `tips::Chords`.

**Spec:** `docs/superpowers/specs/2026-09-27-geode-tile-interaction-doors-design.md`

## Global Constraints

- Crate position: `geode-tile` depends on `geode-shell`, `geode-core`, `gpui`, `gpui-component`; never on `geode-data` or a feature module; `geode-shell` never depends on `geode-tile`.
- `[lib] bench = false`. Dev-dependencies enable exactly the workspace's test features: `geode-core` and `geode-shell` with `test-support`, `gpui` with `test-support` (a mismatch makes `cargo test --workspace` build `geode-shell` twice).
- Theme tokens only: no literal colors or radii; sizes through `shell::scale::design` (rem scale). The two literal pixel values carried over (`SNAP_MARGIN` window clearance, the 2px separator rule) keep a comment saying why.
- No state mutation, I/O or unbounded allocation in render. Menu rows and their key hints are prepared when the menu opens, when its rows are rebuilt, and when the keymap is republished (`Chords` observer) — never in paint. The painter only clones prepared `SharedString`s; its per-render color derivation is a constant handful of contrast checks, the same cost `control::paint` has at every once-per-render site.
- Every pointer action keeps a keyboard route (the menu's row press = `enter`; the confirm's pointer cancel = any non-`y` key).
- A surface dropping a focused handle blurs it first (the confirm's `disarm`/`withdraw`).
- Repeated elements use stable domain-derived IDs: a menu row's element id is its pick's `element_name()`, not its index. Debug selectors keep their existing index-based spellings because tests address them.
- User-facing text says "color"; new code identifiers say `color`.
- Comments state the local invariant and the failure prevented; no task numbers, review ids or dates.
- `docs/current/*` and every touched crate README are updated in the same task as the behavior.
- Mutation harness: mode flags first (`--build-check`, `--anchors-only`); never `--changed`; never an unfiltered mutation run; select by a narrow name substring; before any run, `pgrep -f 'mutation-che[c]k'` must print nothing; commit before any mutation run (the harness edits tracked files); verify each new or re-aimed entry three ways — `zsh scripts/mutation-check.sh --build-check "<substring>"` (no BUILD), `zsh scripts/mutation-check.sh "<substring>"` reports `caught` (not `caught*`), and a hand application of the replacement makes the named test fail on an assertion (then `git checkout -- <file>`). Tests that could hang on a mutant must fail by assertion (no unbounded waits).
- Per-task gates: focused tests; `cargo test -p <crate>` for each touched crate (`cargo test -p geode-app <filter>` for app tests; geode-app is bin-only); `cargo clippy -p <crate> --all-targets -- -D warnings`; `cargo fmt --check`; `zsh scripts/mutation-check.sh --anchors-only`.
- Commit messages end with `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`. A commit that updates a test pinning a deliberately changed behavior names that test in its message.

## Review Focus

1. **A keymap reload while a menu is open.** The trader rebinds a key in the keybindings dialog with a menu up; the open menu's hint must change at once, not at the next open. Pinned by `an_open_menu_follows_a_keymap_reload` in Tasks 4, 5 and 6 and `rehint_follows_a_republished_keymap` in Task 2.
2. **An action the user unbinds entirely.** A verb row (`:upload`, `:price`) falls back to its `:` verb; a key-only row shows an empty lane, never a stale shipped key. Pinned by `a_hint_resolves_to_the_live_chord_or_its_unbound_form` (Task 2) and the market-data rebind test's unbound half (Task 5).
3. **A popup opened near the window's right or bottom edge.** It must stay on screen with the snap margin. Pinned by `a_popup_near_the_edge_snaps_inside_the_margin` (Task 1).
4. **A delivery that withdraws a standing confirm.** No handle is left holding the keyboard and the withdrawn prompt's blur is not heard as a second "no". Pinned by `withdraw_blurs_without_answering` (Task 3) and the existing `a_rebase_under_the_question_withdraws_the_confirm` (Task 5).
5. **A menu rebuilt with fewer rows while its highlight sits past the end.** The highlight must land on an action row (never a separator, never out of range). Pinned by `snap_keeps_or_finds_the_nearest_action` (Task 2) and the existing pricer `a_reload_under_an_open_menu_relists_its_views` (Task 4).

## Spec deviations (code wins; evidence)

1. **Diagnostics has no notice slot.** `crates/geode-diagnostics/src/tile.rs` paints a per-row `Tone` (Normal/Muted/Warn/Error/Marked) for list rows (`tile.rs:583-631`) and returns command refusals to the shell (`tile.rs:505`); there is no header notice. Nothing migrates, and `geode-diagnostics` does not gain a `geode-tile` dependency (an unused dependency). The §3 diagram's arrow to diagnostics is not drawn in `architecture.md`.
2. **Two of §5's three "deliberate menu changes" are not changes.** (a) "Disabled rows show their reason" is current behavior in both modules: pricer `popup.rs:364-367` puts `Err(why)` in the lane; market-data `popup.rs:327-330` paints `Err(r)`. (b) "Stepping from a separator or section lands on the first enabled action" is unreachable through pricer and market-data production routes: the highlight is set only by open (`first_enabled`), hover (handlers exist only on action rows, `popup.rs:381-393` / `:359-374`) and rebuild (pricer `snap`, action rows only). It is pinned at the door (`stepping_from_a_non_action_row_lands_on_the_first_enabled_action`). The only production-visible menu change is (c) key hints following the live keymap, pinned in each module.
3. **Confirm "focus restore" is a blur back to the shell root, not a refocus of the handle recorded at arm time.** Every route that arms a confirm (`:upload`/`:rm` on the tile's `:` line, the palette, a menu row) runs from a shell surface that closes as it dispatches; the handle focused at arm time is that closing input. Refocusing it strands the keyboard on an unpainted element — the failure `docs/current/shell.md` ("Focus") documents. Today both copies blur (`marketdata/src/tile.rs:1357-1367`, `pricer/src/tile.rs:3131-3141`) and the shell's restoration path returns focus to the tile surface; `the_tile_answers_keys_after_the_upload_confirm_ends` pins that. The door keeps it, and its tests assert that no handle holds the keyboard after each answer.
4. **The confirm prompt is not painted in a notice tone.** Both prompts paint `theme.foreground` (`pricer/src/header.rs` prompt, `marketdata/src/header.rs:516` `Tone::Key`); none of `Status/Warning/Danger` is foreground. `confirm::prompt` paints the same one-line run shape as `notice::paint` in `foreground`.
5. **Menu rebuild uses the pricer's `snap`, not timeseries' `step(.., 0)`.** The rebuild rule is not a stepping rule and the spec does not name one. The pricer's is pinned at tile level (`a_reload_under_an_open_menu_relists_its_views` expects the highlight clamped onto the last view, which `step(.., 0)` would move to row 0) and by the harness ("a re-checked menu clamps its highlight"). Timeseries' refresh changes only when a rebuilt menu puts a non-action row under the old index, which its fixed-shape menus do not do.
6. **Timeseries extensions.** `Trailing`, `short_reason` and the non-key `label` are needed by real rows: the range presets' `1w`…`5y` and the frequencies' `1m`…`1w` are non-key lane text (`core/menu.rs:236,273`), and a capped frequency row shows `over cap` while its pick gives the whole cap sentence (`core/menu.rs:275`). They become door fields (`Hint::Label`, `ActionRow::short_reason`, `Trailing`). `start` and `custom_row` stay in timeseries because they match on its own `Pick` variants (module content), rebuilt over door rows.
7. **One renderer unifies small presentation differences** (display checks owed): the lane is `text_sm` everywhere (timeseries used `text_xs`); a lit row's lane uses the lit text color everywhere (the pricer used muted-on-accent); text is floored to the readable ratio on the popover and accent grounds everywhere (only the pricer did); enabled non-lit rows take `control` hover/pressed (timeseries used `listrow` hover; the others had none); section headings ellipsize everywhere. The pricer's lane and section debug selectors (`pricer-menu-lane-{i}`, `pricer-menu-section-{i}`) go; no test reads them.
8. **`row_shell` moves but only timeseries uses it.** The pricer and market-data picker/choice rows have no hover fill today; adopting `row_shell` would add one, a change the spec does not list. They use `popover::surface`, `anchor_popup` and `empty_row`.

## File Structure

New crate `crates/geode-tile/`:

| File | Responsibility |
|---|---|
| `Cargo.toml` | Crate manifest (lib `bench = false`, test-feature parity). |
| `README.md` | Module map, the crate rule, commands. |
| `src/lib.rs` | Crate doc and the four `pub mod`s. |
| `src/notice.rs` | `Notice`, `Tone`, `color`, `paint`, `render`. |
| `src/popover.rs` | Geometry constants, `surface`, `anchor_popup`, `row_shell`, `empty_row`. |
| `src/menu/mod.rs` | Menu model: `MenuPick`, `Hint`, `Unbound`, `Lane`, `Trailing`, `ActionRow`, `Row`, `Menu`, `step`, `first_enabled`, `snap`, `live_bindings`. |
| `src/menu/paint.rs` | `MenuPaint` (floored menu colors, pointer states), `RowPaint`, `row_paint`. |
| `src/menu/render.rs` | `MenuHost`, `MenuIds`, `render_menu`. |
| `src/confirm.rs` | `Confirm`, `ConfirmHost`, `arm`, `key`, `cancel`, `withdraw`, `prompt`, `cancel_on_press`. |

Modified: root `Cargo.toml`, `Cargo.lock`, `CLAUDE.md`, `docs/current/architecture.md`, `docs/current/features.md`, `docs/current/input-and-dialogs.md`, `scripts/mutation-check.sh`; pricer (`Cargo.toml`, `popup.rs`, `paint.rs`, `header.rs`, `tile.rs`, `README.md`), market-data (`Cargo.toml`, `popup.rs`, `core/menu.rs`, `header.rs`, `tile.rs`, `README.md`), timeseries (`Cargo.toml`, `popup.rs`, `core/menu.rs`, `header.rs`, `tile/mod.rs`, `tile/popups.rs`, `tile/tests.rs`, `README.md`), blotter (`Cargo.toml`, `tile.rs`, `README.md`).

## Harness map (every existing entry whose anchor this plan moves or changes)

| Entry (name) | Task | Action |
|---|---|---|
| pricer popup: menu steps land on pickable rows only | 4 | re-aim to `geode-tile/src/menu/mod.rs`, pkg geode-pricer |
| pricer popup: menu steps skip disabled rows | 4 | re-aim to `geode-tile/src/menu/mod.rs`, pkg geode-pricer |
| pricer popup: a disabled menu row takes no fill | 4 | re-aim to `geode-tile/src/menu/paint.rs`, pkg geode-pricer |
| pricer tile: an open menu re-checks its rows on a rebuild | 4 | new anchor in pricer `tile.rs` |
| pricer tile: a re-checked menu clamps its highlight | 4 | re-aim to `geode-tile/src/menu/mod.rs` (`replace_rows`), pkg geode-pricer |
| pricer rm: any key confirms / a modified y confirms | 4 | re-aim to `geode-tile/src/confirm.rs` (`key`), pkg geode-pricer |
| pricer rm: focus leaving leaves the question standing | 4 | re-aim to `confirm.rs` (`arm`'s blur), pkg geode-pricer |
| pricer rm: a pointer press leaves the question standing | 4 | re-aim to `confirm.rs` (`cancel_on_press`), pkg geode-pricer |
| pricer rm: the confirm drops still focused | 4 | re-aim to `confirm.rs` (`disarm`), pkg geode-pricer |
| pricer rm: a key under the confirm reaches the tile too | 4 | re-aim to `confirm.rs` (`prompt`), pkg geode-app (unchanged filter) |
| pricer rm: y forgets nothing; y forgets a sheet opened/retiring since the question; a refused remove loses its kind; the confirm is not insert mode | 4 | unchanged (payload keeps `pending.sheet`; field keeps `confirm`) — `--anchors-only` proves it |
| mdmenu: a greyed row is a notice, not a dispatch | 5 | re-aim to `menu/mod.rs` (`pick`), pkg geode-marketdata |
| mdpark: the menu's load row stays live on a dirty draft | 5 | new anchor in md `core/menu.rs` |
| mdmenu: the popup occludes what is painted beneath it | 5 | re-aim to `menu/render.rs`, pkg geode-marketdata |
| mdmenu: hovering a menu row moves the highlight | 5 | re-aim to `menu/mod.rs` (`highlight`), pkg geode-marketdata |
| mdauto: exactly one policy row is checked | 5 | new anchor in md `core/menu.rs` |
| mdmenu: stepping skips disabled rows / menu_down skips disabled rows in the tile / stepping skips separators and sections | 5 | re-aim to `menu/mod.rs`, pkg geode-marketdata |
| panel: the confirm consumes a non-y key | 5 | re-aim to `confirm.rs` (`prompt`), pkg geode-marketdata |
| panel: a rebase under the confirm withdraws it | 5 | new anchor in md `tile.rs` |
| panel: the tile answers keys after the upload confirm ends | 5 | re-aim to `confirm.rs` (`disarm`), pkg geode-marketdata |
| panel: focus loss cancels the upload confirm | 5 | re-aim to `confirm.rs` (`arm`'s blur), pkg geode-marketdata |
| panel: upload y cancels when the draft changed…; mdupload: an open editor closes…; the other `panel:` upload entries | 5 | unchanged (payload keeps `pending.draft`) |
| timeseries range menu: the preset in force is ticked / an absolute range ticks custom dates | 6 | new anchors in ts `core/menu.rs` |
| timeseries menus: the highlight starts on the value in force | 6 | new anchor in ts `core/menu.rs` |
| timeseries mouse: menu stepping skips non-rows; timeseries menu: stepping skips disabled rows | 6 | re-aim to `menu/mod.rs`, pkg geode-timeseries |
| timeseries menus: a short label is text, not a key | 6 | re-aim to `menu/mod.rs` (`resolve`), pkg geode-timeseries |
| timeseries frequency menu: a capped row shows a short reason / a capped row is disabled | 6 | new anchors in ts `core/menu.rs` |
| timeseries menus: a disabled row explains and stays | 6 | re-aim to `menu/mod.rs` (`pick`), pkg geode-timeseries |
| timeseries dates editor: escape lands the highlight on custom dates | 6 | new anchor in ts `tile/popups.rs` |
| tile: a query error keeps the last snapshot; blotter: a stopped query refusal reads as something else | 7 | new anchors in blotter `tile.rs` |

Entries in files this plan touches whose anchored lines stay byte-identical (for example the timeseries popup outside-press, completion and color-picker entries, `mdmenu: upload is greyed while behind`, `timeseries mouse: an empty tile disables the slot rows`, `timeseries action list: Frequency opens the frequency menu`, `timeseries menus: a chrome rebuild refreshes the open menu's own rows`, `pricer paint:` entries, `mdpaint:` entries) are not listed; each task's `--anchors-only` gate proves they still match exactly once.

**Where new entries go:** immediately above the last `if [[ -n "$changed_ref" ]]; then` line in `scripts/mutation-check.sh` (`grep -n 'if \[\[ -n "\$changed_ref" \]\]; then' scripts/mutation-check.sh | tail -1`). **Re-aiming an entry:** find it with `grep -n 'run_mutation "<name>"' scripts/mutation-check.sh` and replace the whole `run_mutation` block (name line through its filter line) with the block given here; keep the comment above it.

---

### Task 1: Crate skeleton, `notice` and `popover` doors

**Files:**
- Create: `crates/geode-tile/Cargo.toml`, `crates/geode-tile/README.md`, `crates/geode-tile/src/lib.rs`, `crates/geode-tile/src/notice.rs`, `crates/geode-tile/src/popover.rs`
- Modify: `Cargo.toml` (root), `CLAUDE.md`, `docs/current/architecture.md`, `scripts/mutation-check.sh`

**Interfaces:**
- Consumes: `geode_shell::shell::{chip::{self, chip_paint}, kbd, scale}`, `gpui_component::ThemeStyled::popover_style`.
- Produces:
  - `geode_tile::notice::{Tone { Status, Warning, Danger }, Notice}`; `Notice::new(text: impl Into<SharedString>, tone: Tone) -> Notice`, `Notice::status/warning/danger(text) -> Notice`, `Notice::text(&self) -> &SharedString`, `Notice::tone(&self) -> Tone`; `notice::color(tone: Tone, theme: &Theme) -> Hsla`; `notice::paint(text: &SharedString, tone: Tone, theme: &Theme) -> Div`; `notice::render(notice: &Notice, theme: &Theme) -> Div`.
  - `geode_tile::popover::{ROW_HEIGHT, ROW_INSET, MIN_WIDTH, SNAP_MARGIN}: f32`; `popover::surface(cx: &App) -> Div`; `popover::anchor_popup(content: impl IntoElement, corner: Anchor) -> Deferred`; `popover::row_shell(theme: &Theme, hover: Hsla, id: ElementId, highlighted: bool, selector: impl FnOnce() -> String, on_down: impl Fn(&mut Window, &mut App) + 'static) -> Stateful<Div>`; `popover::empty_row(theme: &Theme, text: &'static str) -> Div`.

- [ ] **Step 1: Add the crate to the workspace**

Root `Cargo.toml`: add `"crates/geode-tile",` to `members` directly after `"crates/geode-widgets",`, and add to `[workspace.dependencies]` after the `geode-widgets` line:

```toml
geode-tile = { path = "crates/geode-tile" }
```

Create `crates/geode-tile/Cargo.toml`:

```toml
[package]
name = "geode-tile"
version.workspace = true
edition.workspace = true
publish.workspace = true

[lib]
bench = false

# The kit tile modules are built from. `geode-shell` supplies the paint
# doors these build on (`kbd`, `control`, `scale`, `chip`, `listrow`,
# `colours`) and the live keymap (`tips::Chords`); `geode-core` the
# readability floor. Never `geode-data` and never a feature module; the
# shell never depends on this crate.
[dependencies]
geode-core.workspace = true
geode-shell.workspace = true
gpui.workspace = true
gpui-component.workspace = true

# The same test features the rest of the workspace enables, so
# `cargo test --workspace` builds `geode-shell` once.
[dev-dependencies]
geode-core = { workspace = true, features = ["test-support"] }
geode-shell = { workspace = true, features = ["test-support"] }
gpui = { workspace = true, features = ["test-support"] }
```

Create `crates/geode-tile/src/lib.rs`:

```rust
//! The kit tile modules are built from: a tile mechanism two modules would
//! otherwise each write lives here, interaction behavior (keys, focus, open
//! and close, precedence) as well as paint. The shell hosts tiles and never
//! depends on this crate.

pub mod notice;
pub mod popover;
```

- [ ] **Step 2: Write the failing notice tests**

Create `crates/geode-tile/src/notice.rs` with only the test module first (the module body is added in Step 4):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{Context, IntoElement, Render, TestAppContext, Window};
    use gpui_component::ActiveTheme as _;

    #[gpui::test]
    fn each_tone_paints_its_own_token(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        cx.update(|cx| {
            let theme = cx.theme();
            assert_eq!(color(Tone::Status, theme), theme.muted_foreground);
            assert_eq!(
                color(Tone::Warning, theme),
                chip_paint(theme, chip::Tone::WarningText).text
            );
            assert_eq!(
                color(Tone::Danger, theme),
                chip_paint(theme, chip::Tone::DangerText).text
            );
            assert_ne!(color(Tone::Warning, theme), color(Tone::Danger, theme));
        });
    }

    struct Line(Notice);

    impl Render for Line {
        fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            let tone = self.0.tone();
            render(&self.0, cx.theme()).debug_selector(move || format!("notice-{tone:?}"))
        }
    }

    #[gpui::test]
    fn a_notice_paints_its_text_in_every_tone(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        for n in [
            Notice::status("loading…"),
            Notice::warning("not saved"),
            Notice::danger("failed"),
        ] {
            let tone = n.tone();
            assert!(!n.text().is_empty());
            let (_view, vcx) = cx.add_window_view(|_, _| Line(n));
            vcx.run_until_parked();
            let selector: &'static str = Box::leak(format!("notice-{tone:?}").into_boxed_str());
            assert!(vcx.debug_bounds(selector).is_some(), "{tone:?} paints");
        }
    }
}
```

- [ ] **Step 3: Run it to verify it fails**

Run: `cargo test -p geode-tile notice`
Expected: FAIL to compile — `cannot find function color`, `cannot find type Notice`.

- [ ] **Step 4: Implement `notice`**

Prepend to `crates/geode-tile/src/notice.rs` (above the test module):

```rust
//! A tile's notice: one line of text in one of three tones, painted in theme
//! tokens. A tile with several notice slots (the pricer's pricing, view and
//! save notices; market-data's notice and upload error) decides which one
//! shows; this door paints the winner, so a tone is one color in every tile.

use geode_shell::shell::chip::{self, chip_paint};
use gpui::prelude::*;
use gpui::{Div, Hsla, SharedString, div};
use gpui_component::Theme;

/// What a notice means to the trader.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tone {
    /// Progress with nothing wrong (`loading…`): muted text.
    Status,
    /// Something to know or act on (a dropped selection, a save not
    /// written): warning text.
    Warning,
    /// Something failed or was refused: danger text.
    Danger,
}

/// One notice: its text, prepared when the state changes, and its tone.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Notice {
    text: SharedString,
    tone: Tone,
}

impl Notice {
    pub fn new(text: impl Into<SharedString>, tone: Tone) -> Self {
        Self {
            text: text.into(),
            tone,
        }
    }

    pub fn status(text: impl Into<SharedString>) -> Self {
        Self::new(text, Tone::Status)
    }

    pub fn warning(text: impl Into<SharedString>) -> Self {
        Self::new(text, Tone::Warning)
    }

    pub fn danger(text: impl Into<SharedString>) -> Self {
        Self::new(text, Tone::Danger)
    }

    pub fn text(&self) -> &SharedString {
        &self.text
    }

    pub fn tone(&self) -> Tone {
        self.tone
    }
}

/// A tone's text color. Warning and danger are the shell's floored text
/// tones: the raw `warning`/`danger` tokens fall under the readable ratio as
/// text on several bundled light themes, and the chip door's sweep holds
/// the floored pair to it.
pub fn color(tone: Tone, theme: &Theme) -> Hsla {
    match tone {
        Tone::Status => theme.muted_foreground,
        Tone::Warning => chip_paint(theme, chip::Tone::WarningText).text,
        Tone::Danger => chip_paint(theme, chip::Tone::DangerText).text,
    }
}

/// `text` as one run in `tone`. The caller places it (a header slot, a
/// full-width strip) and may add its own debug selector.
pub fn paint(text: &SharedString, tone: Tone, theme: &Theme) -> Div {
    div().text_color(color(tone, theme)).child(text.clone())
}

/// A prepared [`Notice`] through [`paint`].
pub fn render(notice: &Notice, theme: &Theme) -> Div {
    paint(&notice.text, notice.tone, theme)
}
```

- [ ] **Step 5: Run the notice tests**

Run: `cargo test -p geode-tile notice`
Expected: PASS (2 tests).

- [ ] **Step 6: Write the failing popover tests**

Create `crates/geode-tile/src/popover.rs` with the test module only:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{Context, Pixels, Point, Render, Size, TestAppContext, point};

    /// A popup hung from a point the test chooses from the viewport size.
    struct Probe {
        corner: Anchor,
        at: fn(Size<Pixels>) -> Point<Pixels>,
    }

    impl Render for Probe {
        fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            let at = (self.at)(window.viewport_size());
            div().size_full().child(
                div().absolute().left(at.x).top(at.y).child(anchor_popup(
                    surface(cx)
                        .debug_selector(|| "probe-popup".into())
                        .child("row"),
                    self.corner,
                )),
            )
        }
    }

    fn bounds_of(
        corner: Anchor,
        at: fn(Size<Pixels>) -> Point<Pixels>,
        cx: &mut TestAppContext,
    ) -> (gpui::Bounds<Pixels>, Size<Pixels>) {
        cx.update(gpui_component::init);
        let (_view, vcx) = cx.add_window_view(|_, _| Probe { corner, at });
        vcx.run_until_parked();
        let size = vcx.update(|window, _| window.viewport_size());
        let bounds = vcx.debug_bounds("probe-popup").expect("the popup paints");
        (bounds, size)
    }

    #[gpui::test]
    fn a_popup_hangs_from_its_anchor_corner(cx: &mut TestAppContext) {
        let (b, _) = bounds_of(Anchor::TopRight, |_| point(px(400.), px(40.)), cx);
        assert!(
            (b.top_right().x - px(400.)).abs() <= px(0.5),
            "the popup's right edge is on the anchor: {b:?}"
        );
        assert!((b.top_right().y - px(40.)).abs() <= px(0.5), "{b:?}");
    }

    #[gpui::test]
    fn a_popup_near_the_edge_snaps_inside_the_margin(cx: &mut TestAppContext) {
        let (b, size) = bounds_of(
            Anchor::TopLeft,
            |size| point(size.width - px(20.), px(40.)),
            cx,
        );
        let limit = size.width - px(SNAP_MARGIN);
        assert!(
            (b.top_right().x - limit).abs() <= px(0.5),
            "snapped to the margin, not the window edge: {b:?} in {size:?}"
        );
    }
}
```

- [ ] **Step 7: Run it to verify it fails**

Run: `cargo test -p geode-tile popover`
Expected: FAIL to compile — `cannot find function anchor_popup`, `surface`, `SNAP_MARGIN`.

- [ ] **Step 8: Implement `popover`**

Prepend to `crates/geode-tile/src/popover.rs`. `row_shell` and `empty_row` are moved verbatim from `crates/geode-timeseries/src/popup.rs:607-648` (made `pub`); do not change their bodies:

```rust
//! Geometry and layering for a tile's anchored popups: the popover surface,
//! the deferred anchor that lifts a popup above the tile's clip and keeps
//! it on screen, and the common row frame. What a popup lists is the
//! module's content; this door owns where and how it floats.

use geode_shell::shell::{kbd, scale};
use gpui::prelude::*;
use gpui::{
    Anchor, AnchoredPositionMode, App, Deferred, Div, ElementId, Hsla, MouseButton, Stateful,
    Window, anchored, deferred, div, px,
};
use gpui_component::{Theme, ThemeStyled as _, h_flex, v_flex};

/// Popup-row height in design pixels, scaled with the shell's rem size.
pub const ROW_HEIGHT: f32 = 26.0;
/// Horizontal row inset in design pixels.
pub const ROW_INSET: f32 = 8.0;
/// The popup's minimum width at the design rem: room for a title and a
/// trailing key lane.
pub const MIN_WIDTH: f32 = 240.0;
/// Least distance, in window pixels, a snapped popup keeps from the window
/// edge. Not rem-scaled: it is clearance from the window frame, not content.
pub const SNAP_MARGIN: f32 = 8.0;

/// The popover treatment every tile popup shares: scaled minimum width,
/// inset and row spacing.
pub fn surface(cx: &App) -> Div {
    v_flex()
        .min_w(scale::design(MIN_WIDTH))
        .p_1()
        .gap_y_0p5()
        .text_sm()
        .popover_style(cx)
}

/// Hang `content` by its own `corner` from the zero-size point the caller
/// paints this at. `deferred` paints it above later siblings and outside the
/// tile's and table's clips (priority 1, over the tile's own deferred
/// elements); the snap keeps a popup opened near an edge on screen. A popup
/// painted without the snap runs off the window when opened from a bottom
/// row or the right edge.
pub fn anchor_popup(content: impl IntoElement, corner: Anchor) -> Deferred {
    deferred(
        anchored()
            .anchor(corner)
            .position_mode(AnchoredPositionMode::Local)
            .snap_to_window_with_margin(px(SNAP_MARGIN))
            .child(content),
    )
    .with_priority(1)
}

/// Shared list-row geometry, selection colors, and left-press handling.
/// Unselected rows show the supplied hover fill without moving a cursor.
/// The press is consumed before the callback runs, so the surface beneath
/// cannot also act on it.
///
/// The caller supplies a per-popup row id and derives `hover` once per
/// popup paint, avoiding a contrast calculation per row.
pub fn row_shell(
    theme: &Theme,
    hover: Hsla,
    id: ElementId,
    highlighted: bool,
    selector: impl FnOnce() -> String,
    on_down: impl Fn(&mut Window, &mut App) + 'static,
) -> Stateful<Div> {
    h_flex()
        .id(id)
        .h(scale::design(ROW_HEIGHT))
        .px(scale::design(ROW_INSET))
        .gap_2()
        .rounded(theme.radius)
        .items_center()
        .when(highlighted, |d| {
            d.bg(theme.accent).text_color(theme.accent_foreground)
        })
        .when(!highlighted, |d| {
            d.text_color(theme.popover_foreground)
                .hover(move |s| s.bg(hover))
        })
        .debug_selector(selector)
        .on_mouse_down(MouseButton::Left, move |_, window, cx| {
            cx.stop_propagation();
            on_down(window, cx);
        })
}

/// The "nothing here" row: muted, the same height and inset as a row, with
/// backtick-quoted runs painted as keys.
pub fn empty_row(theme: &Theme, text: &'static str) -> Div {
    div()
        .h(scale::design(ROW_HEIGHT))
        .px(scale::design(ROW_INSET))
        .flex()
        .items_center()
        .text_color(theme.muted_foreground)
        .child(kbd::marked(text))
}
```

These two bodies are byte-for-byte the ones in `crates/geode-timeseries/src/popup.rs:607-648` (only `fn` → `pub fn` and the doc wording); Task 6 deletes the originals.

- [ ] **Step 9: Run the popover tests**

Run: `cargo test -p geode-tile popover`
Expected: PASS (2 tests).

- [ ] **Step 10: README, CLAUDE.md and architecture**

Create `crates/geode-tile/README.md`:

````markdown
# geode-tile

The kit tile modules are built from. A tile mechanism two modules would
otherwise each write lives here, and that covers interaction behavior (keys,
focus, open and close, precedence), not only paint. `geode-shell` hosts
tiles and never depends on this crate; this crate never depends on
`geode-data` or a feature module.

Current architecture:
[`docs/current/architecture.md`](../../docs/current/architecture.md).

## Modules

| Module | Holds |
|---|---|
| `notice` | `Notice` (prepared text and a `Tone`: `Status`, `Warning`, `Danger`) and its one paint in theme tokens. Precedence between a tile's notice slots stays the tile's. |
| `popover` | Popup geometry (`ROW_HEIGHT`, `ROW_INSET`, `MIN_WIDTH`, `SNAP_MARGIN`), the popover `surface`, `anchor_popup` (deferred, anchored, snapped, priority 1), and the `row_shell`/`empty_row` row frames. |

## Commands

```sh
cargo test -p geode-tile
```
````

`CLAUDE.md`, section "Dependency and ownership rules": after the line beginning `- Feature modules do not depend on sibling features.` insert:

```markdown
- `geode-tile` sits between `geode-shell` and the feature modules. It depends
  on `geode-shell` and `geode-core`, never on `geode-data` or a feature
  module; `geode-shell` never depends on it.
```

`CLAUDE.md`, section "UI and GPUI rules": after the line beginning `- Dialogs open through` insert:

```markdown
- A tile mechanism two modules would otherwise each write lives in
  `geode-tile` — interaction behavior (keys, focus, open and close,
  precedence) as well as paint: popups (`popover`), `.` action menus
  (`menu`), the in-tile y/n confirm (`confirm`) and notices (`notice`).
```

`docs/current/architecture.md`: replace the dependency diagram block with:

```text
                         geode-app
                 composition and process setup
                    /         |          \
             feature modules  |       geode-data
               /      \       |           |
        geode-tile  shared widgets    geode-core
               |        |                 /
        geode-shell     |                /
               \        |               /
                └──── geode-core ──────┘

calculation leaf: geode-pricing ─────────────────► geode-core
pure presentation: geode-chart, geode-widgets ───► geode-core
wire formats: geode-documents ───────────────────► geode-core
```

and after the paragraph that begins `` `geode-shell` owns the window and interaction model.`` add:

```markdown
`geode-tile` is the kit tiles are built from: the popover, the `.` action
menu, the in-tile y/n confirm and the notice line, as models with one
painter each. It depends on `geode-shell` for its paint doors and the live
keymap, never on `geode-data` or a feature module, and the shell never
depends on it. A tile mechanism two modules would otherwise each write lives
there.
```

- [ ] **Step 11: Harness entries for the two doors**

Insert above the last `if [[ -n "$changed_ref" ]]; then` in `scripts/mutation-check.sh`:

```sh
# ---- geode-tile: notice and popover doors ------------------------------
#
# A tone is one color in every tile. Mutated, a warning paints as danger.
run_mutation "tile notice: warning paints the danger token" \
  crates/geode-tile/src/notice.rs \
  '        Tone::Warning => chip_paint(theme, chip::Tone::WarningText).text,' \
  '        Tone::Warning => chip_paint(theme, chip::Tone::DangerText).text,' \
  geode-tile each_tone_paints_its_own_token

# A popup hangs by the corner its caller names.
run_mutation "tile popover: a popup ignores its corner" \
  crates/geode-tile/src/popover.rs \
  '            .anchor(corner)' \
  '            .anchor(Anchor::TopLeft)' \
  geode-tile a_popup_hangs_from_its_anchor_corner

# A popup near the window edge keeps the snap margin.
run_mutation "tile popover: a popup snaps to the window edge itself" \
  crates/geode-tile/src/popover.rs \
  '            .snap_to_window_with_margin(px(SNAP_MARGIN))' \
  '            .snap_to_window_with_margin(px(0.))' \
  geode-tile a_popup_near_the_edge_snaps_inside_the_margin
```

- [ ] **Step 12: Gates**

Run:
```sh
cargo test -p geode-tile
cargo clippy -p geode-tile --all-targets -- -D warnings
cargo fmt --check
zsh scripts/mutation-check.sh --anchors-only
```
Expected: all pass; `--anchors-only` exits 0.

- [ ] **Step 13: Commit**

```bash
git add Cargo.toml Cargo.lock CLAUDE.md docs/current/architecture.md crates/geode-tile scripts/mutation-check.sh
git commit -m "feat(tile): geode-tile crate with notice and popover doors

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

- [ ] **Step 14: Verify the three new entries**

```sh
pgrep -f 'mutation-che[c]k'            # must print nothing
zsh scripts/mutation-check.sh --build-check "tile notice:"
zsh scripts/mutation-check.sh --build-check "tile popover:"
zsh scripts/mutation-check.sh "tile notice:"
zsh scripts/mutation-check.sh "tile popover:"
```
Expected: no BUILD; three `caught` lines. Then hand-apply each replacement in turn, run its named test (`cargo test -p geode-tile <filter>`), confirm it fails on an `assert`, and `git checkout -- crates/geode-tile/src`.

---

### Task 2: `menu` door (model, stepping, live hints, renderer)

**Files:**
- Create: `crates/geode-tile/src/menu/mod.rs`, `crates/geode-tile/src/menu/paint.rs`, `crates/geode-tile/src/menu/render.rs`
- Modify: `crates/geode-tile/src/lib.rs`, `crates/geode-tile/README.md`, `scripts/mutation-check.sh`

**Interfaces:**
- Consumes: Task 1 `popover::{surface, anchor_popup, ROW_HEIGHT, ROW_INSET}`; `geode_shell::tips::{Chords, chord_for}`; `geode_shell::keymap::{Binding, Keystroke, Modifiers, parse_binding}`; `geode_shell::shell::{kbd::menu_binding, control, colours, scale}`.
- Produces (all in `geode_tile::menu`):
  - `trait MenuPick: Clone + PartialEq + Debug + 'static { fn element_name(&self) -> SharedString; }`, implemented here for `geode_shell::actions::ActionId`.
  - `enum Hint { None, Chord { action: SharedString, unbound: Unbound }, Label(SharedString) }` with `Hint::chord(&'static str)`, `Hint::chord_or_verb(&'static str, &'static str)`, `Hint::chord_or_keys(&'static str, &'static str)`, `Hint::label(&'static str)`.
  - `enum Unbound { Blank, Verb(SharedString), Keys(Vec<Keystroke>) }`; `enum Lane { Empty, Keys(Vec<Keystroke>), Text(SharedString) }`; `enum Trailing<'a> { None, Keys(&'a [Keystroke]), Text(&'a SharedString) }`.
  - `struct ActionRow<P>`: `ActionRow::new(pick: P, title: impl Into<SharedString>) -> Self`; builders `.hint(Hint)`, `.enabled(Result<(), SharedString>)`, `.short_reason(&'static str)`, `.checked(bool)`; readers `.pick() -> &P`, `.name() -> &SharedString`, `.title() -> &SharedString`, `.is_enabled() -> bool`, `.reason() -> Option<&SharedString>`, `.checked() -> Option<bool>`, `.lane() -> &Lane`, `.trailing() -> Trailing<'_>`.
  - `enum Row<P> { Action(ActionRow<P>), Separator, Section(SharedString) }` with `.action() -> Option<&ActionRow<P>>`, `.is_action() -> bool`, `.lands() -> bool`.
  - `fn first_enabled<P>(&[Row<P>]) -> Option<usize>`; `fn step<P>(&[Row<P>], Option<usize>, isize) -> Option<usize>`; `fn snap<P>(&[Row<P>], Option<usize>) -> Option<usize>`.
  - `struct Menu<P>`: `Menu::new(rows: Vec<Row<P>>, bindings: &[Binding]) -> Self` (hints resolved, highlight on the first enabled action); `.open_at(Option<usize>) -> Self`; `.rows() -> &[Row<P>]`; `.highlighted() -> Option<usize>`; `.step(isize)`; `.highlight(usize) -> bool`; `.pick(usize) -> Option<Result<P, SharedString>>`; `.replace_rows(Vec<Row<P>>, &[Binding]) -> bool`; `.rehint(&[Binding])`.
  - `fn live_bindings(cx: &App) -> Arc<Vec<Binding>>`.
  - `struct MenuPaint { pub text, pub muted, pub active_fill, pub active_text: Hsla, pub rest_pointer: ControlPaint }`, `MenuPaint::derive(&Theme) -> MenuPaint`; `struct RowPaint { pub fill: Option<Hsla>, pub text: Hsla, pub lane: Hsla, pub pointer: Option<ControlPaint> }`; `fn row_paint(&MenuPaint, lit: bool, enabled: bool) -> RowPaint`.
  - `trait MenuHost: Sized + 'static { fn menu_pick(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>); fn menu_hover(&mut self, index: usize, cx: &mut Context<Self>); }`; `struct MenuIds`, `MenuIds::new(menu: impl Into<SharedString>, row: impl Into<SharedString>)` (row `i` paints debug selector `"{row}-{i}"`); `fn render_menu<P: MenuPick, T: MenuHost>(menu: &Menu<P>, ids: &MenuIds, corner: Anchor, tile: &Entity<T>, on_outside: impl Fn(&mut T, &mut Window, &mut Context<T>) + 'static, cx: &App) -> Deferred`.

- [ ] **Step 1: Write the failing pure model tests**

Create `crates/geode-tile/src/menu/mod.rs` with this test module (the body comes in Step 3):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use geode_core::config::{Layer, LayerDoc};
    use geode_shell::actions::{ActionDef, ActionRegistry};
    use geode_shell::defaults::default_mod;
    use geode_shell::keymap::build_keymap;

    #[derive(Clone, Debug, PartialEq)]
    pub(crate) struct Id(pub &'static str);

    impl MenuPick for Id {
        fn element_name(&self) -> SharedString {
            SharedString::new_static(self.0)
        }
    }

    fn act(id: &'static str) -> Row<Id> {
        Row::Action(ActionRow::new(Id(id), id))
    }

    fn off(id: &'static str) -> Row<Id> {
        Row::Action(ActionRow::new(Id(id), id).enabled(Err("not now".into())))
    }

    /// 0 a, 1 —, 2 b (disabled), 3 c, 4 [View], 5 d.
    fn mixed() -> Vec<Row<Id>> {
        vec![
            act("a"),
            Row::Separator,
            off("b"),
            act("c"),
            Row::Section("View".into()),
            act("d"),
        ]
    }

    #[test]
    fn stepping_skips_separators_and_sections_and_clamps() {
        let rows = vec![act("a"), Row::Separator, act("b"), Row::Section("S".into()), act("c")];
        assert_eq!(step(&rows, Some(0), 1), Some(2));
        assert_eq!(step(&rows, Some(2), 1), Some(4), "over the section");
        assert_eq!(step(&rows, Some(4), 1), Some(4), "clamped at the end");
        assert_eq!(step(&rows, Some(4), -2), Some(0));
        assert_eq!(step(&rows, Some(0), -1), Some(0), "clamped at the start");
        assert_eq!(step(&rows, Some(0), 7), Some(4));
    }

    #[test]
    fn stepping_skips_disabled_actions() {
        let rows = mixed();
        assert_eq!(step(&rows, Some(0), 1), Some(3), "over the separator and b");
        assert_eq!(step(&rows, Some(3), -1), Some(0));
        assert_eq!(step(&rows, Some(2), 1), Some(3), "from a pointer-lit disabled row");
        assert_eq!(step(&rows, Some(2), -1), Some(0));
        assert_eq!(step(&rows, Some(2), 0), Some(2), "a zero step keeps an action row");
    }

    #[test]
    fn stepping_from_a_non_action_row_lands_on_the_first_enabled_action() {
        let rows = mixed();
        assert_eq!(step(&rows, Some(1), 1), Some(0), "from the separator");
        assert_eq!(step(&rows, Some(4), -1), Some(0), "from the section");
        assert_eq!(step(&rows, None, 1), Some(0), "from no cursor");
        assert_eq!(step(&rows, Some(99), 1), Some(0), "from past the end");
    }

    #[test]
    fn an_all_disabled_menu_has_no_cursor() {
        let rows = vec![Row::Section("S".into()), off("a"), Row::Separator, off("b")];
        assert_eq!(first_enabled(&rows), None);
        assert_eq!(step(&rows, None, 1), None);
        let menu = Menu::new(rows, &[]);
        assert_eq!(menu.highlighted(), None);
    }

    #[test]
    fn snap_keeps_or_finds_the_nearest_action() {
        let rows = mixed();
        assert_eq!(snap(&rows, Some(3)), Some(3), "an action row stays");
        assert_eq!(snap(&rows, Some(2)), Some(2), "a disabled action stays");
        assert_eq!(snap(&rows, Some(4)), Some(3), "a section gives way upward");
        assert_eq!(snap(&rows, Some(99)), Some(5), "past the end clamps to the last row");
        assert_eq!(snap(&rows, None), Some(0));
        let lead = vec![Row::Section("S".into()), act("a")];
        assert_eq!(snap(&lead, Some(0)), Some(1), "nothing before it: the next one");
    }

    #[test]
    fn highlight_moves_only_onto_action_rows_and_reports_a_change() {
        let mut menu = Menu::new(mixed(), &[]);
        assert_eq!(menu.highlighted(), Some(0));
        assert!(!menu.highlight(0), "no change, no report");
        assert!(!menu.highlight(1), "a separator takes no highlight");
        assert!(!menu.highlight(4), "a section takes no highlight");
        assert!(menu.highlight(2), "a disabled action does (a hover is a hover)");
        assert_eq!(menu.highlighted(), Some(2));
    }

    #[test]
    fn a_pick_of_a_disabled_row_is_its_reason() {
        let menu = Menu::new(mixed(), &[]);
        assert_eq!(menu.pick(0), Some(Ok(Id("a"))));
        assert_eq!(menu.pick(2), Some(Err(SharedString::from("not now"))));
        assert_eq!(menu.pick(1), None, "structure picks nothing");
        assert_eq!(menu.pick(99), None);
    }

    fn registry(ids: &[&str]) -> ActionRegistry {
        let mut r = ActionRegistry::default();
        for id in ids {
            r.register(ActionDef {
                id: ActionId(id.to_string()),
                title: id.to_string(),
                category: "Demo".into(),
            })
            .unwrap();
        }
        r
    }

    pub(crate) fn doc(layer: Layer, text: &str) -> LayerDoc {
        LayerDoc {
            layer,
            name: "keymap".to_string(),
            file: format!("{}/keymap.toml", layer.name()).into(),
            table: text.parse().unwrap(),
        }
    }

    pub(crate) fn bindings(user: Option<&str>) -> Vec<Binding> {
        let mut docs = vec![doc(
            Layer::Builtin,
            "[[bindings]]\n[bindings.keys]\n\"a\" = \"demo::alpha\"\n",
        )];
        if let Some(text) = user {
            docs.push(doc(Layer::User, text));
        }
        let (keymap, diags) = build_keymap(&docs, default_mod(), &registry(&["demo::alpha"]));
        assert!(diags.is_empty(), "{diags:?}");
        keymap.bindings().to_vec()
    }

    fn keys(spec: &str) -> Vec<Keystroke> {
        parse_binding(spec, Modifiers::NONE).unwrap()
    }

    #[test]
    fn a_hint_resolves_to_the_live_chord_or_its_unbound_form() {
        let rows = vec![
            Row::Action(ActionRow::new(Id("a"), "Alpha").hint(Hint::chord("demo::alpha"))),
            Row::Action(ActionRow::new(Id("v"), "Verb").hint(Hint::chord_or_verb("demo::verb", ":verb"))),
            Row::Action(ActionRow::new(Id("c"), "Custom").hint(Hint::chord_or_keys("demo::custom", "c"))),
            Row::Action(ActionRow::new(Id("u"), "Unbound").hint(Hint::chord("demo::unbound"))),
            Row::Action(ActionRow::new(Id("l"), "1 week").hint(Hint::label("1w"))),
        ];
        let menu = Menu::new(rows, &bindings(None));
        let lanes: Vec<Lane> = menu
            .rows()
            .iter()
            .map(|r| r.action().unwrap().lane().clone())
            .collect();
        assert_eq!(
            lanes,
            vec![
                Lane::Keys(keys("a")),
                Lane::Text(":verb".into()),
                Lane::Keys(keys("c")),
                Lane::Empty,
                Lane::Text("1w".into()),
            ]
        );
    }

    #[test]
    fn a_disabled_row_trails_its_short_reason_else_its_reason() {
        let short = ActionRow::new(Id("f"), "1 minute")
            .hint(Hint::label("1m"))
            .enabled(Err("1m over 1y is 525,600 points; the cap is 500,000".into()))
            .short_reason("over cap");
        let long = ActionRow::new(Id("g"), "Group").enabled(Err("not in a package".into()));
        let label = ActionRow::new(Id("l"), "1 week").hint(Hint::label("1w"));
        let menu = Menu::new(
            vec![Row::Action(short), Row::Action(long), Row::Action(label)],
            &[],
        );
        let trail = |i: usize| match menu.rows()[i].action().unwrap().trailing() {
            Trailing::Text(t) => t.to_string(),
            Trailing::Keys(k) => format!("{k:?}"),
            Trailing::None => String::new(),
        };
        assert_eq!(trail(0), "over cap");
        assert_eq!(trail(1), "not in a package");
        assert_eq!(trail(2), "1w", "a label is text, not a key");
        assert_eq!(
            menu.pick(0),
            Some(Err(SharedString::from(
                "1m over 1y is 525,600 points; the cap is 500,000"
            ))),
            "the pick gives the whole reason"
        );
    }

    #[test]
    fn replace_rows_resolves_hints_and_snaps_the_highlight() {
        let mut menu = Menu::new(mixed(), &[]);
        assert!(menu.highlight(5));
        let fewer = vec![act("a"), Row::Separator, act("c")];
        assert!(menu.replace_rows(fewer.clone(), &[]), "the rows moved");
        assert_eq!(menu.highlighted(), Some(2), "clamped onto the last action");
        assert!(!menu.replace_rows(fewer, &[]), "the same rows did not move");
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p geode-tile menu`
Expected: FAIL to compile (`menu` module not declared). Add `pub mod menu;` to `crates/geode-tile/src/lib.rs` (between `pub mod notice;` and `pub mod popover;` ordering: keep alphabetical — `menu`, `notice`, `popover`) and rerun: FAIL — `cannot find type Row`, `Menu`, `step`…

- [ ] **Step 3: Implement the model**

Prepend to `crates/geode-tile/src/menu/mod.rs`:

```rust
//! One model and one renderer for a tile's `.` action menu.
//!
//! Rows are built by the module when the menu opens, when its content
//! changes, and when the keymap is republished — never in paint. A row's
//! key hint is an action identity resolved to keystrokes through the live
//! keymap (`tips::Chords`) at that moment, so a user rebind shows. A
//! disabled row keeps its reason: the lane shows it (or its short form) and a
//! pick returns it for the tile's notice. Keyboard stepping lands only on
//! enabled actions; from a separator, a section or no cursor it lands on the
//! first enabled action. An all-disabled menu has no cursor.
//!
//! Which keys step and pick stays the module's (its own `menu_down`,
//! `menu_up`, `menu_pick` bindings); the module maps them onto
//! [`Menu::step`] and [`Menu::pick`].

mod paint;
mod render;

pub use paint::{MenuPaint, RowPaint, row_paint};
pub use render::{MenuHost, MenuIds, render_menu};

use std::sync::Arc;

use geode_shell::actions::ActionId;
use geode_shell::keymap::{Binding, Keystroke, Modifiers, parse_binding};
use geode_shell::tips::{Chords, chord_for};
use gpui::{App, SharedString};

/// A module's pick type: what a row does when picked. `element_name` names
/// the row's element; it must be unique within one menu and stable across
/// rebuilds of the same row, so a rebuilt menu keeps each row's hover state.
pub trait MenuPick: Clone + PartialEq + std::fmt::Debug + 'static {
    fn element_name(&self) -> SharedString;
}

impl MenuPick for ActionId {
    fn element_name(&self) -> SharedString {
        SharedString::from(self.0.clone())
    }
}

/// What an enabled row's trailing lane names, before the keymap is read.
#[derive(Clone, Debug, PartialEq)]
pub enum Hint {
    /// Nothing trails the title.
    None,
    /// The action's live chord; `unbound` when the keymap binds none.
    Chord {
        action: SharedString,
        unbound: Unbound,
    },
    /// Text that is not a key (a preset's `1w`): painted as text.
    Label(SharedString),
}

impl Hint {
    /// The live chord, or an empty lane when unbound.
    pub fn chord(action: &'static str) -> Hint {
        Hint::Chord {
            action: SharedString::new_static(action),
            unbound: Unbound::Blank,
        }
    }

    /// The live chord, or the `:` verb that reaches the same action.
    pub fn chord_or_verb(action: &'static str, verb: &'static str) -> Hint {
        Hint::Chord {
            action: SharedString::new_static(action),
            unbound: Unbound::Verb(SharedString::new_static(verb)),
        }
    }

    /// The live chord, or `shipped` (a keymap spelling) for a key the
    /// surface handles itself when the keymap binds the action nowhere.
    pub fn chord_or_keys(action: &'static str, shipped: &'static str) -> Hint {
        Hint::Chord {
            action: SharedString::new_static(action),
            unbound: Unbound::Keys(
                parse_binding(shipped, Modifiers::NONE).expect("a shipped hint spells a key"),
            ),
        }
    }

    pub fn label(text: &'static str) -> Hint {
        Hint::Label(SharedString::new_static(text))
    }
}

/// What a [`Hint::Chord`] shows when the keymap binds the action nowhere.
#[derive(Clone, Debug, PartialEq)]
pub enum Unbound {
    Blank,
    Verb(SharedString),
    Keys(Vec<Keystroke>),
}

/// A hint resolved against the keymap: what an enabled row's lane holds.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum Lane {
    #[default]
    Empty,
    Keys(Vec<Keystroke>),
    Text(SharedString),
}

/// What a row's trailing lane paints this frame: keys as `Kbd`, or text.
#[derive(Clone, Debug, PartialEq)]
pub enum Trailing<'a> {
    None,
    Keys(&'a [Keystroke]),
    Text(&'a SharedString),
}

/// One pickable row of a menu.
#[derive(Clone, Debug, PartialEq)]
pub struct ActionRow<P> {
    pick: P,
    name: SharedString,
    title: SharedString,
    hint: Hint,
    lane: Lane,
    enabled: Result<(), SharedString>,
    short_reason: Option<SharedString>,
    checked: Option<bool>,
}

impl<P: MenuPick> ActionRow<P> {
    pub fn new(pick: P, title: impl Into<SharedString>) -> Self {
        let name = pick.element_name();
        Self {
            pick,
            name,
            title: title.into(),
            hint: Hint::None,
            lane: Lane::Empty,
            enabled: Ok(()),
            short_reason: None,
            checked: None,
        }
    }
}

impl<P> ActionRow<P> {
    pub fn hint(mut self, hint: Hint) -> Self {
        self.hint = hint;
        self
    }

    /// `Err` is why the row cannot be picked: the lane shows it and a pick
    /// returns it.
    pub fn enabled(mut self, enabled: Result<(), SharedString>) -> Self {
        self.enabled = enabled;
        self
    }

    /// A disabled row's lane text when its reason is a sentence too long for
    /// the lane; a pick still returns the whole reason.
    pub fn short_reason(mut self, reason: &'static str) -> Self {
        self.short_reason = Some(SharedString::new_static(reason));
        self
    }

    /// A toggle's or a choice's state: a tick, or a same-width blank, ahead
    /// of the title so a group's titles align.
    pub fn checked(mut self, on: bool) -> Self {
        self.checked = Some(on);
        self
    }

    pub fn pick(&self) -> &P {
        &self.pick
    }

    pub fn name(&self) -> &SharedString {
        &self.name
    }

    pub fn title(&self) -> &SharedString {
        &self.title
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled.is_ok()
    }

    pub fn reason(&self) -> Option<&SharedString> {
        self.enabled.as_ref().err()
    }

    pub fn checked(&self) -> Option<bool> {
        self.checked
    }

    pub fn lane(&self) -> &Lane {
        &self.lane
    }

    pub fn trailing(&self) -> Trailing<'_> {
        match (&self.enabled, &self.lane) {
            (Err(reason), _) => Trailing::Text(self.short_reason.as_ref().unwrap_or(reason)),
            (Ok(()), Lane::Empty) => Trailing::None,
            (Ok(()), Lane::Keys(keys)) => Trailing::Keys(keys),
            (Ok(()), Lane::Text(text)) => Trailing::Text(text),
        }
    }
}

/// One row of a menu.
#[derive(Clone, Debug, PartialEq)]
pub enum Row<P> {
    Action(ActionRow<P>),
    Separator,
    /// A muted heading over the rows that follow.
    Section(SharedString),
}

impl<P> Row<P> {
    pub fn action(&self) -> Option<&ActionRow<P>> {
        match self {
            Row::Action(a) => Some(a),
            _ => None,
        }
    }

    pub fn is_action(&self) -> bool {
        matches!(self, Row::Action(_))
    }

    /// Whether keyboard stepping may land here: an enabled action.
    pub fn lands(&self) -> bool {
        matches!(self, Row::Action(a) if a.enabled.is_ok())
    }
}

/// The first row keyboard stepping may land on; `None` when every action is
/// disabled.
pub fn first_enabled<P>(rows: &[Row<P>]) -> Option<usize> {
    rows.iter().position(Row::lands)
}

/// Move `delta` enabled actions from `from`, skipping disabled actions,
/// separators and sections, clamped at either end. From a row that is not
/// an action (or no row), land on the first enabled action. From a disabled
/// action the pointer left lit, search from there; with nothing enabled in
/// the requested direction, stay.
pub fn step<P>(rows: &[Row<P>], from: Option<usize>, delta: isize) -> Option<usize> {
    let Some(from) = from.filter(|&i| rows.get(i).is_some_and(Row::is_action)) else {
        return first_enabled(rows);
    };
    let mut at = from;
    for _ in 0..delta.unsigned_abs() {
        let next = if delta > 0 {
            (at + 1..rows.len()).find(|&i| rows[i].lands())
        } else {
            (0..at).rev().find(|&i| rows[i].lands())
        };
        match next {
            Some(i) => at = i,
            None => break,
        }
    }
    Some(at)
}

/// Where a highlight goes when a menu's rows are rebuilt under it: kept on an
/// action row, else the nearest action before it, else after it. Disabled
/// actions qualify (a pointer may have lit one). Past the end clamps first.
pub fn snap<P>(rows: &[Row<P>], at: Option<usize>) -> Option<usize> {
    let Some(at) = at else {
        return first_enabled(rows);
    };
    let at = at.min(rows.len().saturating_sub(1));
    (0..=at)
        .rev()
        .find(|&i| rows.get(i).is_some_and(Row::is_action))
        .or_else(|| (at..rows.len()).find(|&i| rows[i].is_action()))
}

fn resolve(hint: &Hint, bindings: &[Binding]) -> Lane {
    match hint {
        Hint::None => Lane::Empty,
        Hint::Label(text) => Lane::Text(text.clone()),
        Hint::Chord { action, unbound } => match chord_for(bindings, action) {
            Some(keys) => Lane::Keys(keys),
            None => match unbound {
                Unbound::Blank => Lane::Empty,
                Unbound::Verb(verb) => Lane::Text(verb.clone()),
                Unbound::Keys(keys) => Lane::Keys(keys.clone()),
            },
        },
    }
}

fn resolve_all<P>(rows: &mut [Row<P>], bindings: &[Binding]) {
    for row in rows {
        if let Row::Action(a) = row {
            a.lane = resolve(&a.hint, bindings);
        }
    }
}

/// An open menu: its prepared rows and the one highlight keyboard and
/// pointer share.
#[derive(Clone, Debug, PartialEq)]
pub struct Menu<P> {
    rows: Vec<Row<P>>,
    highlighted: Option<usize>,
}

impl<P: MenuPick> Menu<P> {
    /// `rows` with their hints resolved against `bindings`, the highlight on
    /// the first enabled action.
    pub fn new(mut rows: Vec<Row<P>>, bindings: &[Binding]) -> Self {
        resolve_all(&mut rows, bindings);
        let highlighted = first_enabled(&rows);
        Self { rows, highlighted }
    }

    /// Open with the highlight on `at` (a module's own start rule, such as
    /// the value in force) when it is an action row.
    pub fn open_at(mut self, at: Option<usize>) -> Self {
        if let Some(i) = at.filter(|&i| self.rows.get(i).is_some_and(Row::is_action)) {
            self.highlighted = Some(i);
        }
        self
    }

    pub fn rows(&self) -> &[Row<P>] {
        &self.rows
    }

    pub fn highlighted(&self) -> Option<usize> {
        self.highlighted
    }

    pub fn step(&mut self, delta: isize) {
        self.highlighted = step(&self.rows, self.highlighted, delta);
    }

    /// The pointer's row: the mouse form of stepping. Only action rows take
    /// it (disabled ones too: a hover is a hover, and a pick on one gives its
    /// reason). Answers whether it moved, so a caller notifies only on a
    /// change: gpui fires a row's mouse-move on every pointer move over it.
    pub fn highlight(&mut self, index: usize) -> bool {
        if self.highlighted == Some(index) || !self.rows.get(index).is_some_and(Row::is_action) {
            return false;
        }
        self.highlighted = Some(index);
        true
    }

    /// Row `index` picked: `Ok` with its pick when enabled, `Err` with its
    /// whole reason when disabled (never the pick: a disabled row must not
    /// also run the verb its reason refuses), `None` for structure.
    pub fn pick(&self, index: usize) -> Option<Result<P, SharedString>> {
        let Some(Row::Action(action)) = self.rows.get(index) else {
            return None;
        };
        Some(match &action.enabled {
            Ok(()) => Ok(action.pick.clone()),
            Err(reason) => Err(reason.clone()),
        })
    }

    /// Rows rebuilt under an open menu (a delivery, a reload, a `:` line):
    /// hints resolved, the highlight snapped. Answers whether the rows moved.
    pub fn replace_rows(&mut self, mut rows: Vec<Row<P>>, bindings: &[Binding]) -> bool {
        resolve_all(&mut rows, bindings);
        let moved = rows != self.rows;
        self.rows = rows;
        self.highlighted = snap(&self.rows, self.highlighted);
        moved
    }

    /// Re-resolve every hint against a republished keymap.
    pub fn rehint(&mut self, bindings: &[Binding]) {
        resolve_all(&mut self.rows, bindings);
    }
}

/// The keymap as the shell last published it; empty before it does (a
/// test with no `Chords`, a module constructed before the shell's first
/// publish). Cloning the `Arc` copies no binding.
pub fn live_bindings(cx: &App) -> Arc<Vec<Binding>> {
    cx.try_global::<Chords>()
        .map(|c| Arc::clone(&c.0))
        .unwrap_or_default()
}
```

- [ ] **Step 4: Run the model tests**

`mod.rs` now declares `mod paint;` and `mod render;`, so the crate compiles only once both exist: write Step 5 (`paint.rs`, complete with its tests) and Step 7 (`render.rs`'s implementation, above an empty `#[cfg(test)] mod tests {}` that Step 6 fills), then run:

Run: `cargo test -p geode-tile menu::tests`
Expected: PASS (11 tests).

- [ ] **Step 5: Write the paint module with its tests**

Create `crates/geode-tile/src/menu/paint.rs`:

```rust
//! A menu's colors, derived from the theme: text floored to the readable
//! ratio on the popover and on the lit row's accent, and the pointer states
//! of an enabled row that is not lit. Floored toward whichever of black or
//! white contrasts more with the ground: floored toward the color itself, a
//! color equal to its ground has nothing to move toward and stays unreadable.

use geode_core::colour::{Rgb, contrast_ratio, readable_on};
use geode_shell::shell::colours::{over, to_hsla, to_rgb};
use geode_shell::shell::control::{self, ControlPaint, Rest};
use gpui::Hsla;
use gpui_component::Theme;

const BLACK: Rgb = Rgb {
    r: 0.0,
    g: 0.0,
    b: 0.0,
};
const WHITE: Rgb = Rgb {
    r: 1.0,
    g: 1.0,
    b: 1.0,
};

fn pole(ground: Rgb) -> Rgb {
    if contrast_ratio(BLACK, ground) >= contrast_ratio(WHITE, ground) {
        BLACK
    } else {
        WHITE
    }
}

fn floor(color: Hsla, ground: Rgb) -> Hsla {
    to_hsla(readable_on(to_rgb(color), ground, pole(ground)))
}

/// A menu's prepared colors.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MenuPaint {
    /// An enabled row's title on the popover.
    pub text: Hsla,
    /// Lanes, section headings, and a disabled row's title, on the popover.
    pub muted: Hsla,
    /// The lit row's fill.
    pub active_fill: Hsla,
    /// The lit row's title and lane on its fill.
    pub active_text: Hsla,
    /// Hover and pressed for an enabled row that is not lit.
    pub rest_pointer: ControlPaint,
}

impl MenuPaint {
    pub fn derive(theme: &Theme) -> MenuPaint {
        let (popover, active) = Self::grounds(theme);
        let text = floor(theme.popover_foreground, popover);
        MenuPaint {
            text,
            muted: floor(theme.muted_foreground, popover),
            active_fill: theme.accent,
            active_text: floor(theme.accent_foreground, active),
            rest_pointer: control::paint(theme, Rest::Bare, theme.popover, text),
        }
    }

    /// The popover over the window background, and the lit row's accent over
    /// that: the two grounds a menu's text lands on.
    pub(crate) fn grounds(theme: &Theme) -> (Rgb, Rgb) {
        let popover = over(theme.popover, to_rgb(theme.background));
        (popover, over(theme.accent, popover))
    }
}

/// One action row's paint.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RowPaint {
    pub fill: Option<Hsla>,
    pub text: Hsla,
    pub lane: Hsla,
    /// `None` on a lit row (the pointer's row is the lit row, since a hover
    /// moves the highlight; a second fill there would recolor the highlight
    /// under the mouse) and on a disabled row (it takes no fill at all).
    pub pointer: Option<ControlPaint>,
}

/// Only a lit, enabled row takes the fill. A disabled row can hold the
/// highlight (the pointer lit it) but stays muted on the popover: a fill
/// there reads as an offer on a row that will only refuse. The lit row's
/// lane follows its title so keys never sit muted on the accent.
pub fn row_paint(p: &MenuPaint, lit: bool, enabled: bool) -> RowPaint {
    match (lit, enabled) {
        (true, true) => RowPaint {
            fill: Some(p.active_fill),
            text: p.active_text,
            lane: p.active_text,
            pointer: None,
        },
        (false, true) => RowPaint {
            fill: None,
            text: p.text,
            lane: p.muted,
            pointer: Some(p.rest_pointer),
        },
        (_, false) => RowPaint {
            fill: None,
            text: p.muted,
            lane: p.muted,
            pointer: None,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use geode_core::colour::READABLE_RATIO;
    use gpui_component::ActiveTheme as _;

    #[gpui::test]
    fn only_an_enabled_lit_row_takes_the_fill(cx: &mut gpui::TestAppContext) {
        cx.update(gpui_component::init);
        cx.update(|cx| {
            let p = MenuPaint::derive(cx.theme());
            assert_eq!(row_paint(&p, true, true).fill, Some(p.active_fill));
            assert_eq!(row_paint(&p, true, true).lane, p.active_text);
            assert_eq!(row_paint(&p, false, true).fill, None);
            assert_eq!(row_paint(&p, false, true).pointer, Some(p.rest_pointer));
            assert_eq!(row_paint(&p, true, false).fill, None);
            assert_eq!(row_paint(&p, true, false).text, p.muted);
            assert_eq!(row_paint(&p, true, false).pointer, None);
            assert_eq!(row_paint(&p, false, false), row_paint(&p, true, false));
        });
    }

    #[test]
    fn a_color_equal_to_its_ground_floors_to_the_readable_ratio() {
        for level in [0.2, 0.5, 0.8] {
            let ground = Rgb {
                r: level,
                g: level,
                b: level,
            };
            let floored = floor(to_hsla(ground), ground);
            assert!(
                contrast_ratio(to_rgb(floored), ground) >= READABLE_RATIO,
                "a {level} grey on itself must reach {READABLE_RATIO}:1"
            );
        }
    }

    #[gpui::test]
    fn every_menu_paint_is_readable_on_every_bundled_theme(cx: &mut gpui::TestAppContext) {
        cx.update(gpui_component::init);
        let (service, _) = geode_shell::theme::load_bundled();
        let mut failures = Vec::new();
        let mut checked = 0;
        for name in service.names() {
            let entry = service.resolve(&name).unwrap().clone();
            cx.update(|cx| {
                Theme::global_mut(cx).apply_config(&entry);
                let theme = cx.theme();
                let p = MenuPaint::derive(theme);
                let (popover, active) = MenuPaint::grounds(theme);
                for (label, text, ground) in [
                    ("text", p.text, popover),
                    ("muted", p.muted, popover),
                    ("active text", p.active_text, active),
                ] {
                    checked += 1;
                    let ratio = contrast_ratio(to_rgb(text), ground);
                    if ratio < READABLE_RATIO {
                        failures.push(format!("{name}: {label} at {ratio:.2}:1"));
                    }
                }
            });
        }
        assert!(checked >= 3 * 40, "every bundled theme was swept ({checked})");
        assert!(failures.is_empty(), "unreadable menu paints:\n{}", failures.join("\n"));
    }
}
```

- [ ] **Step 6: Write the failing GPUI menu tests (rebind, rehint, render)**

Create `crates/geode-tile/src/menu/render.rs` with only this test module first:

```rust
#[cfg(test)]
mod tests {
    use super::super::tests::{Id, bindings};
    use super::super::*;
    use super::*;
    use geode_shell::tips::Chords;
    use gpui::{Modifiers, Point, Render, TestAppContext, VisualTestContext, px};
    use std::sync::Arc;

    fn rows() -> Vec<Row<Id>> {
        vec![
            Row::Action(ActionRow::new(Id("a"), "Alpha").hint(Hint::chord("demo::alpha"))),
            Row::Separator,
            Row::Action(ActionRow::new(Id("b"), "Beta").enabled(Err("not now".into()))),
            Row::Action(ActionRow::new(Id("c"), "Gamma")),
        ]
    }

    const REBOUND: &str =
        "[[bindings]]\n[bindings.keys]\n\"a\" = \"none\"\n\"shift+a\" = \"demo::alpha\"\n";

    fn alpha_lane(menu: &Menu<Id>) -> Lane {
        menu.rows()[0].action().unwrap().lane().clone()
    }

    fn keys(spec: &str) -> Vec<geode_shell::keymap::Keystroke> {
        geode_shell::keymap::parse_binding(spec, geode_shell::keymap::Modifiers::NONE).unwrap()
    }

    #[gpui::test]
    fn a_hint_follows_a_user_rebind_through_the_live_keymap(cx: &mut TestAppContext) {
        cx.update(|cx| cx.set_global(Chords(Arc::new(bindings(Some(REBOUND))))));
        let menu = cx.update(|cx| Menu::new(rows(), &live_bindings(cx)));
        assert_eq!(alpha_lane(&menu), Lane::Keys(keys("shift+a")));
    }

    #[gpui::test]
    fn rehint_follows_a_republished_keymap(cx: &mut TestAppContext) {
        cx.update(|cx| cx.set_global(Chords(Arc::new(bindings(None)))));
        let mut menu = cx.update(|cx| Menu::new(rows(), &live_bindings(cx)));
        assert_eq!(alpha_lane(&menu), Lane::Keys(keys("a")));
        cx.update(|cx| cx.set_global(Chords(Arc::new(bindings(Some(REBOUND))))));
        cx.update(|cx| menu.rehint(&live_bindings(cx)));
        assert_eq!(alpha_lane(&menu), Lane::Keys(keys("shift+a")));
    }

    struct Probe {
        menu: Menu<Id>,
        ids: MenuIds,
        picks: Vec<Result<Id, SharedString>>,
        outside: usize,
        under: usize,
    }

    impl MenuHost for Probe {
        fn menu_pick(&mut self, index: usize, _: &mut Window, cx: &mut Context<Self>) {
            if let Some(p) = self.menu.pick(index) {
                self.picks.push(p);
            }
            cx.notify();
        }

        fn menu_hover(&mut self, index: usize, cx: &mut Context<Self>) {
            if self.menu.highlight(index) {
                cx.notify();
            }
        }
    }

    impl Render for Probe {
        fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            let tile = cx.entity();
            div()
                .size_full()
                .child(
                    div()
                        .id("under")
                        .absolute()
                        .size_full()
                        .on_mouse_move(cx.listener(|this, _, _, cx| {
                            this.under += 1;
                            cx.notify();
                        })),
                )
                .child(div().absolute().top(px(10.)).left(px(10.)).child(render_menu(
                    &self.menu,
                    &self.ids,
                    Anchor::TopLeft,
                    &tile,
                    |t: &mut Probe, _, cx| {
                        t.outside += 1;
                        cx.notify();
                    },
                    cx,
                )))
        }
    }

    fn open(cx: &mut TestAppContext) -> (Entity<Probe>, &mut VisualTestContext) {
        cx.update(gpui_component::init);
        let (probe, vcx) = cx.add_window_view(|_, _| Probe {
            menu: Menu::new(rows(), &[]),
            ids: MenuIds::new("probe-menu", "probe-menu-row"),
            picks: Vec::new(),
            outside: 0,
            under: 0,
        });
        vcx.run_until_parked();
        (probe, vcx)
    }

    fn centre_of(vcx: &mut VisualTestContext, selector: &str) -> Point<gpui::Pixels> {
        vcx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        let selector: &'static str = Box::leak(selector.to_string().into_boxed_str());
        vcx.debug_bounds(selector)
            .unwrap_or_else(|| panic!("{selector} is painted"))
            .center()
    }

    #[gpui::test]
    fn a_hover_lights_its_row_and_the_menu_occludes_what_it_covers(cx: &mut TestAppContext) {
        let (probe, vcx) = open(cx);
        let at = centre_of(vcx, "probe-menu-row-3");
        vcx.simulate_mouse_move(at, None, Modifiers::default());
        probe.read_with(vcx, |p, _| {
            assert_eq!(p.menu.highlighted(), Some(3));
            assert_eq!(p.under, 0, "the covered element heard no move");
        });
        let beside = centre_of(vcx, "probe-menu") + gpui::point(px(600.), px(0.));
        vcx.simulate_mouse_move(beside, None, Modifiers::default());
        probe.read_with(vcx, |p, _| assert!(p.under > 0, "fixture: the counter hears moves"));
    }

    #[gpui::test]
    fn a_row_press_picks_a_disabled_one_gives_its_reason_and_outside_closes(
        cx: &mut TestAppContext,
    ) {
        let (probe, vcx) = open(cx);
        let row = centre_of(vcx, "probe-menu-row-0");
        vcx.simulate_click(row, Modifiers::default());
        let off = centre_of(vcx, "probe-menu-row-2");
        vcx.simulate_click(off, Modifiers::default());
        let beside = centre_of(vcx, "probe-menu") + gpui::point(px(600.), px(0.));
        vcx.simulate_click(beside, Modifiers::default());
        probe.read_with(vcx, |p, _| {
            assert_eq!(
                p.picks,
                vec![Ok(Id("a")), Err(SharedString::from("not now"))]
            );
            assert_eq!(p.outside, 1);
        });
    }
}
```

- [ ] **Step 7: Implement the renderer**

Prepend to `crates/geode-tile/src/menu/render.rs`:

```rust
//! The one menu painter. It reads the prepared [`Menu`] and formats
//! nothing but the lazy debug selectors.

use super::{Menu, MenuPick, Row, Trailing, paint::{MenuPaint, row_paint}};
use crate::popover::{self, ROW_HEIGHT, ROW_INSET};
use geode_shell::shell::control::PointerStates as _;
use geode_shell::shell::{kbd, scale};
use gpui::prelude::*;
use gpui::{
    Anchor, AnyElement, App, Context, Deferred, ElementId, Entity, MouseButton, SharedString,
    Window, div, px,
};
use gpui_component::{ActiveTheme as _, h_flex};

/// The leading tick slot on a checked row: the same width ticked or not, so
/// a group's titles share one leading edge.
const TICK_SLOT: f32 = 14.0;

/// The tile a menu is painted for: where a row press and a row hover go.
pub trait MenuHost: Sized + 'static {
    /// A row press, and the module's `enter`: one path.
    fn menu_pick(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>);
    /// The pointer resting on row `index`: the mouse form of stepping.
    fn menu_hover(&mut self, index: usize, cx: &mut Context<Self>);
}

/// A menu's element names, prepared once: the container's selector and id,
/// and the row selector prefix (row `i` is `"{row}-{i}"`).
#[derive(Clone, Debug, PartialEq)]
pub struct MenuIds {
    menu: SharedString,
    row: SharedString,
}

impl MenuIds {
    pub fn new(menu: impl Into<SharedString>, row: impl Into<SharedString>) -> Self {
        Self {
            menu: menu.into(),
            row: row.into(),
        }
    }
}

/// Paint `menu` hung by its `corner` from the point the caller paints this
/// at. A row press picks through [`MenuHost::menu_pick`] and stops there: a
/// pick must not also reach the tile beneath (which would, say, cancel its
/// editor). A press outside runs `on_outside`. Hover moves the highlight.
pub fn render_menu<P: MenuPick, T: MenuHost>(
    menu: &Menu<P>,
    ids: &MenuIds,
    corner: Anchor,
    tile: &Entity<T>,
    on_outside: impl Fn(&mut T, &mut Window, &mut Context<T>) + 'static,
    cx: &App,
) -> Deferred {
    let theme = cx.theme();
    let paint = MenuPaint::derive(theme);
    let menu_selector = ids.menu.clone();
    let mut list = popover::surface(cx)
        .id(ElementId::Name(ids.menu.clone()))
        .debug_selector(move || menu_selector.to_string())
        // Occlude what the menu covers so its hover and presses do not also reach it.
        .occlude()
        .on_mouse_down_out({
            let tile = tile.clone();
            move |_, window, cx| tile.update(cx, |t, cx| on_outside(t, window, cx))
        });
    for (i, row) in menu.rows().iter().enumerate() {
        list = list.child(match row {
            // gpui-component's `PopupMenu` separator: a rule bleeding into the
            // surface's inset, half a step of air either side; two pixels so
            // it reads at every rem size.
            Row::Separator => div()
                .my_0p5()
                .mx_neg_1()
                .border_b(px(2.))
                .border_color(theme.border)
                .into_any_element(),
            Row::Section(title) => div()
                .px(scale::design(ROW_INSET))
                .pt_1()
                .text_xs()
                .text_color(paint.muted)
                .overflow_hidden()
                .text_ellipsis()
                .child(title.clone())
                .into_any_element(),
            Row::Action(action) => {
                let rp = row_paint(&paint, menu.highlighted() == Some(i), action.is_enabled());
                // Two static strings: a frame formats nothing here.
                let tick: Option<&'static str> =
                    action.checked().map(|on| if on { "\u{2713}" } else { "" });
                let trailing: AnyElement = match action.trailing() {
                    Trailing::Keys(keys) => kbd::menu_binding(keys, rp.lane).into_any_element(),
                    Trailing::Text(text) => div().child(text.clone()).into_any_element(),
                    Trailing::None => div().into_any_element(),
                };
                let row_selector = ids.row.clone();
                h_flex()
                    .id(ElementId::Name(action.name().clone()))
                    .h(scale::design(ROW_HEIGHT))
                    .px(scale::design(ROW_INSET))
                    .rounded(theme.radius)
                    .items_center()
                    .justify_between()
                    .gap_4()
                    .when_some(rp.fill, |d, fill| d.bg(fill))
                    .text_color(rp.text)
                    .when_some(rp.pointer, |d, states| d.pointer_states(states))
                    .debug_selector(move || format!("{row_selector}-{i}"))
                    .on_mouse_down(MouseButton::Left, {
                        let tile = tile.clone();
                        move |_, window, cx| {
                            cx.stop_propagation();
                            tile.update(cx, |t, cx| t.menu_pick(i, window, cx))
                        }
                    })
                    .on_mouse_move({
                        let tile = tile.clone();
                        move |_, _, cx| tile.update(cx, |t, cx| t.menu_hover(i, cx))
                    })
                    .child(
                        h_flex()
                            .gap_1()
                            .when_some(tick, |d, tick| {
                                d.child(div().w(scale::design(TICK_SLOT)).flex_shrink_0().child(tick))
                            })
                            .child(action.title().clone()),
                    )
                    .child(div().text_color(rp.lane).child(trailing))
                    .into_any_element()
            }
        });
    }
    popover::anchor_popup(list, corner)
}
```

- [ ] **Step 8: Run all menu tests**

Run: `cargo test -p geode-tile menu`
Expected: PASS (11 model + 3 paint + 4 render tests).

- [ ] **Step 9: README**

Add to the module table in `crates/geode-tile/README.md` (alphabetical, above `notice`):

```markdown
| `menu` | The `.` action menu: `Row` (`Action`/`Separator`/`Section`), `ActionRow` (pick, title, `Hint` resolved to a `Lane` through the live keymap, `enabled` with its reason, optional short reason, `checked`), `Menu` (highlight, `step`, `highlight`, `pick`, `replace_rows`, `rehint`), and `render_menu` over a `MenuHost`. Stepping lands only on enabled actions, and from a non-action row on the first enabled one; an all-disabled menu has no cursor; a rebuild snaps the highlight to the nearest action. Which keys step and pick stays the module's. |
```

and a section after the table:

```markdown
## Menu hints

A row's hint is an action identity, not a string. `Menu::new`,
`replace_rows` and `rehint` resolve it against the bindings they are given —
modules pass `menu::live_bindings(cx)`, the shell's last `Chords` publish —
so a user rebind shows. A module re-resolves an open menu from its `Chords`
observer. An action bound nowhere shows its `Unbound` form: nothing, its `:`
verb, or a key its surface handles itself.
```

- [ ] **Step 10: Harness entries**

Insert above the last `if [[ -n "$changed_ref" ]]; then`:

```sh
# ---- geode-tile: menu door ---------------------------------------------
#
# Stepping lands on actions only: a highlight on structure makes `enter`
# pick nothing.
run_mutation "tile menu: stepping lands on separators" \
  crates/geode-tile/src/menu/mod.rs \
  '            (at + 1..rows.len()).find(|&i| rows[i].lands())' \
  '            (at + 1..rows.len()).next()' \
  geode-tile stepping_skips_separators_and_sections_and_clamps

# Nor on a disabled action: `j` lands on a greyed row that only refuses.
run_mutation "tile menu: stepping lands on disabled rows" \
  crates/geode-tile/src/menu/mod.rs \
  '            (at + 1..rows.len()).find(|&i| rows[i].lands())' \
  '            (at + 1..rows.len()).find(|&i| rows[i].is_action())' \
  geode-tile stepping_skips_disabled_actions

# From a separator, a section or no cursor, a step lands on the first
# enabled action.
run_mutation "tile menu: a step from structure searches from it" \
  crates/geode-tile/src/menu/mod.rs \
  '    let Some(from) = from.filter(|&i| rows.get(i).is_some_and(Row::is_action)) else {' \
  '    let Some(from) = from else {' \
  geode-tile stepping_from_a_non_action_row_lands_on_the_first_enabled_action

# An all-disabled menu has no cursor.
run_mutation "tile menu: an all-disabled menu gets a cursor" \
  crates/geode-tile/src/menu/mod.rs \
  '    rows.iter().position(Row::lands)' \
  '    rows.iter().position(Row::is_action)' \
  geode-tile an_all_disabled_menu_has_no_cursor

# A rebuilt menu's highlight looks back before it looks forward.
run_mutation "tile menu: snap only looks forward" \
  crates/geode-tile/src/menu/mod.rs \
  '        .find(|&i| rows.get(i).is_some_and(Row::is_action))
        .or_else(|| (at..rows.len()).find(|&i| rows[i].is_action()))' \
  '        .find(|_| false)
        .or_else(|| (at..rows.len()).find(|&i| rows[i].is_action()))' \
  geode-tile snap_keeps_or_finds_the_nearest_action

# A hint is the live chord: mutated, every chord row falls to its unbound form.
run_mutation "tile menu: a hint ignores the live keymap" \
  crates/geode-tile/src/menu/mod.rs \
  '        Hint::Chord { action, unbound } => match chord_for(bindings, action) {' \
  '        Hint::Chord { action: _, unbound } => match None::<Vec<Keystroke>> {' \
  geode-tile a_hint_resolves_to_the_live_chord_or_its_unbound_form

run_mutation "tile menu: a rebind does not reach the hint" \
  crates/geode-tile/src/menu/mod.rs \
  '        Hint::Chord { action, unbound } => match chord_for(bindings, action) {' \
  '        Hint::Chord { action: _, unbound } => match None::<Vec<Keystroke>> {' \
  geode-tile a_hint_follows_a_user_rebind_through_the_live_keymap

# An unbound verb row names its `:` verb.
run_mutation "tile menu: an unbound verb paints nothing" \
  crates/geode-tile/src/menu/mod.rs \
  '                Unbound::Verb(verb) => Lane::Text(verb.clone()),' \
  '                Unbound::Verb(_) => Lane::Empty,' \
  geode-tile a_hint_resolves_to_the_live_chord_or_its_unbound_form

# A label is text in the lane; routed to the key lane it would paint nothing.
run_mutation "tile menu: a label paints nothing" \
  crates/geode-tile/src/menu/mod.rs \
  '        Hint::Label(text) => Lane::Text(text.clone()),' \
  '        Hint::Label(_) => Lane::Empty,' \
  geode-tile a_hint_resolves_to_the_live_chord_or_its_unbound_form

# A capped row keeps the lane narrow with its short reason.
run_mutation "tile menu: a short reason is ignored" \
  crates/geode-tile/src/menu/mod.rs \
  '            (Err(reason), _) => Trailing::Text(self.short_reason.as_ref().unwrap_or(reason)),' \
  '            (Err(reason), _) => Trailing::Text(reason),' \
  geode-tile a_disabled_row_trails_its_short_reason_else_its_reason

# A disabled row's pick is its reason, never the verb it refuses.
run_mutation "tile menu: a disabled pick dispatches" \
  crates/geode-tile/src/menu/mod.rs \
  '            Err(reason) => Err(reason.clone()),' \
  '            Err(_) => Ok(action.pick.clone()),' \
  geode-tile a_pick_of_a_disabled_row_is_its_reason

# Hover lands on action rows only.
run_mutation "tile menu: hover lights structure" \
  crates/geode-tile/src/menu/mod.rs \
  '        if self.highlighted == Some(index) || !self.rows.get(index).is_some_and(Row::is_action) {' \
  '        if self.highlighted == Some(index) {' \
  geode-tile highlight_moves_only_onto_action_rows_and_reports_a_change

# A rebuild snaps the highlight.
run_mutation "tile menu: a rebuild keeps an out-of-range highlight" \
  crates/geode-tile/src/menu/mod.rs \
  '        self.highlighted = snap(&self.rows, self.highlighted);' \
  '' \
  geode-tile replace_rows_resolves_hints_and_snaps_the_highlight

# A disabled row takes no fill.
run_mutation "tile menu: a disabled row takes the fill" \
  crates/geode-tile/src/menu/paint.rs \
  '        (_, false) => RowPaint {
            fill: None,' \
  '        (_, false) => RowPaint {
            fill: Some(p.active_fill),' \
  geode-tile only_an_enabled_lit_row_takes_the_fill

# The floor moves toward the pole with more contrast.
run_mutation "tile menu: the floor moves toward the weaker pole" \
  crates/geode-tile/src/menu/paint.rs \
  '    if contrast_ratio(BLACK, ground) >= contrast_ratio(WHITE, ground) {' \
  '    if contrast_ratio(BLACK, ground) < contrast_ratio(WHITE, ground) {' \
  geode-tile a_color_equal_to_its_ground_floors_to_the_readable_ratio

# The menu occludes what it covers.
run_mutation "tile menu: the menu does not occlude" \
  crates/geode-tile/src/menu/render.rs \
  '        // Occlude what the menu covers so its hover and presses do not also reach it.
        .occlude()' \
  '        // Occlude what the menu covers so its hover and presses do not also reach it.' \
  geode-tile a_hover_lights_its_row_and_the_menu_occludes_what_it_covers
```

- [ ] **Step 11: Gates**

```sh
cargo test -p geode-tile
cargo clippy -p geode-tile --all-targets -- -D warnings
cargo fmt --check
zsh scripts/mutation-check.sh --anchors-only
```
Expected: all pass.

- [ ] **Step 12: Commit, then verify the entries**

```bash
git add crates/geode-tile scripts/mutation-check.sh
git commit -m "feat(tile): menu door with live key hints and one renderer

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

```sh
pgrep -f 'mutation-che[c]k'
zsh scripts/mutation-check.sh --build-check "tile menu:"
zsh scripts/mutation-check.sh "tile menu:"
```
Expected: no BUILD; 16 `caught`. Hand-apply each replacement, run its named test, confirm an assertion failure, restore with `git checkout -- crates/geode-tile/src`.

---

### Task 3: `confirm` door

**Files:**
- Create: `crates/geode-tile/src/confirm.rs`
- Modify: `crates/geode-tile/src/lib.rs` (`pub mod confirm;` first in the list), `crates/geode-tile/README.md`, `scripts/mutation-check.sh`

**Interfaces:**
- Consumes: nothing from earlier tasks.
- Produces (in `geode_tile::confirm`):
  - `struct Confirm<P>` with `.payload() -> &P`, `.prompt_text() -> &SharedString`, `.focus_handle() -> &FocusHandle`, `.holds_focus(&Window) -> bool`.
  - `trait ConfirmHost: Sized + 'static { type Payload: 'static; fn confirm_slot(&mut self) -> &mut Option<Confirm<Self::Payload>>; fn confirmed(&mut self, payload: Self::Payload, window: &mut Window, cx: &mut Context<Self>); fn cancelled(&mut self, payload: Self::Payload, window: &mut Window, cx: &mut Context<Self>); }`.
  - `fn arm<T: ConfirmHost>(host: &mut T, payload: T::Payload, prompt: impl Into<SharedString>, window: &mut Window, cx: &mut Context<T>)`.
  - `fn key<T: ConfirmHost>(host: &mut T, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<T>) -> bool` (answers whether it consumed the key).
  - `fn cancel<T: ConfirmHost>(host: &mut T, window: &mut Window, cx: &mut Context<T>) -> bool`.
  - `fn withdraw<T: ConfirmHost>(host: &mut T, cx: &mut Context<T>) -> Option<T::Payload>` (no `Window`; no `cancelled` call).
  - `fn prompt<T: ConfirmHost>(confirm: &Confirm<T::Payload>, tile: &Entity<T>, selector: impl FnOnce() -> String + 'static, theme: &Theme) -> Div`.
  - `fn cancel_on_press<E: InteractiveElement + FluentBuilder, T: ConfirmHost>(root: E, armed: bool, tile: &Entity<T>) -> E`.

- [ ] **Step 1: Write the failing GPUI tests**

Create `crates/geode-tile/src/confirm.rs` with the test module:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{Modifiers, Render, TestAppContext, VisualTestContext, px};
    use gpui_component::{ActiveTheme as _, v_flex};

    struct Probe {
        confirm: Option<Confirm<&'static str>>,
        confirmed: Vec<&'static str>,
        cancelled: Vec<&'static str>,
        /// Keys that reached the probe's root past the prompt.
        leaked: Vec<String>,
    }

    impl ConfirmHost for Probe {
        type Payload = &'static str;
        fn confirm_slot(&mut self) -> &mut Option<Confirm<&'static str>> {
            &mut self.confirm
        }
        fn confirmed(&mut self, p: &'static str, _: &mut Window, cx: &mut Context<Self>) {
            self.confirmed.push(p);
            cx.notify();
        }
        fn cancelled(&mut self, p: &'static str, _: &mut Window, cx: &mut Context<Self>) {
            self.cancelled.push(p);
            cx.notify();
        }
    }

    impl Render for Probe {
        fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            let tile = cx.entity();
            let root = v_flex()
                .size_full()
                .on_key_down(cx.listener(|this, e: &KeyDownEvent, _, _| {
                    this.leaked.push(e.keystroke.key.clone())
                }))
                .child(
                    div()
                        .debug_selector(|| "probe-body".into())
                        .h(px(40.))
                        .w_full()
                        .child("body"),
                )
                .when_some(self.confirm.as_ref(), |el, c| {
                    el.child(prompt(c, &tile, || "probe-prompt".into(), cx.theme()))
                });
            cancel_on_press(root, self.confirm.is_some(), &tile)
        }
    }

    fn open(cx: &mut TestAppContext) -> (Entity<Probe>, &mut VisualTestContext) {
        cx.update(gpui_component::init);
        let (probe, vcx) = cx.add_window_view(|_, _| Probe {
            confirm: None,
            confirmed: Vec::new(),
            cancelled: Vec::new(),
            leaked: Vec::new(),
        });
        vcx.update(|window, _| window.activate_window());
        vcx.run_until_parked();
        (probe, vcx)
    }

    fn draw(vcx: &mut VisualTestContext) {
        vcx.run_until_parked();
        vcx.update(|window, cx| {
            let _ = window.draw(cx);
        });
    }

    fn arm_probe(probe: &Entity<Probe>, payload: &'static str, vcx: &mut VisualTestContext) -> FocusHandle {
        vcx.update(|window, cx| {
            probe.update(cx, |p, cx| arm(p, payload, "go? (y/n)", window, cx))
        });
        draw(vcx);
        let focus = probe.read_with(vcx, |p, _| p.confirm.as_ref().expect("armed").focus_handle().clone());
        assert!(vcx.update(|window, _| focus.is_focused(window)), "the prompt holds the keyboard");
        focus
    }

    /// The keyboard went back: the prompt's handle is not focused and no
    /// other handle was focused in its place (the shell's restoration path
    /// returns focus to the tile surface from here).
    fn gave_the_keyboard_back(focus: &FocusHandle, vcx: &mut VisualTestContext) {
        assert!(!vcx.update(|window, _| focus.is_focused(window)), "blurred before it dropped");
        assert!(vcx.update(|window, cx| window.focused(cx).is_none()));
    }

    #[gpui::test]
    fn y_confirms_once_and_gives_the_keyboard_back(cx: &mut TestAppContext) {
        let (probe, vcx) = open(cx);
        let focus = arm_probe(&probe, "x", vcx);
        vcx.simulate_keystrokes("y");
        draw(vcx);
        probe.read_with(vcx, |p, _| {
            assert_eq!(p.confirmed, vec!["x"]);
            assert!(p.cancelled.is_empty());
            assert!(p.confirm.is_none());
            assert!(p.leaked.is_empty(), "the answer was the confirm's alone: {:?}", p.leaked);
        });
        gave_the_keyboard_back(&focus, vcx);
        vcx.simulate_keystrokes("y");
        draw(vcx);
        probe.read_with(vcx, |p, _| assert_eq!(p.confirmed, vec!["x"], "a later y answers nothing"));
    }

    #[gpui::test]
    fn any_other_key_cancels_and_is_consumed(cx: &mut TestAppContext) {
        let (probe, vcx) = open(cx);
        for key in ["n", "escape", "j", "shift-y", "ctrl-y"] {
            let focus = arm_probe(&probe, "x", vcx);
            vcx.simulate_keystrokes(key);
            draw(vcx);
            probe.read_with(vcx, |p, _| {
                assert!(p.confirmed.is_empty(), "{key} confirmed");
                assert!(p.confirm.is_none(), "{key}");
                assert!(p.leaked.is_empty(), "{key} reached the root: {:?}", p.leaked);
            });
            gave_the_keyboard_back(&focus, vcx);
        }
        probe.read_with(vcx, |p, _| assert_eq!(p.cancelled.len(), 5));
    }

    #[gpui::test]
    fn a_pointer_press_cancels(cx: &mut TestAppContext) {
        let (probe, vcx) = open(cx);
        for selector in ["probe-body", "probe-prompt"] {
            let focus = arm_probe(&probe, "x", vcx);
            let at = vcx.debug_bounds(selector).expect("painted").center();
            vcx.simulate_click(at, Modifiers::default());
            draw(vcx);
            probe.read_with(vcx, |p, _| {
                assert!(p.confirm.is_none(), "a press on {selector} cancels");
                assert!(p.confirmed.is_empty());
            });
            gave_the_keyboard_back(&focus, vcx);
        }
        probe.read_with(vcx, |p, _| assert_eq!(p.cancelled, vec!["x", "x"]));
    }

    #[gpui::test]
    fn a_blur_cancels(cx: &mut TestAppContext) {
        let (probe, vcx) = open(cx);
        let focus = arm_probe(&probe, "x", vcx);
        vcx.update(|window, cx| window.blur(cx));
        draw(vcx);
        probe.read_with(vcx, |p, _| {
            assert_eq!(p.cancelled, vec!["x"]);
            assert!(p.confirm.is_none());
        });
        gave_the_keyboard_back(&focus, vcx);
        vcx.simulate_keystrokes("y");
        draw(vcx);
        probe.read_with(vcx, |p, _| assert!(p.confirmed.is_empty(), "a later y answers nothing"));
    }

    #[gpui::test]
    fn withdraw_blurs_without_answering(cx: &mut TestAppContext) {
        let (probe, vcx) = open(cx);
        let focus = arm_probe(&probe, "x", vcx);
        vcx.update(|_, cx| {
            probe.update(cx, |p, cx| assert_eq!(withdraw(p, cx), Some("x")))
        });
        draw(vcx);
        probe.read_with(vcx, |p, _| {
            assert!(p.confirm.is_none());
            assert!(p.cancelled.is_empty(), "the withdrawn prompt's blur is not a no");
            assert!(p.confirmed.is_empty());
        });
        gave_the_keyboard_back(&focus, vcx);
    }

    #[gpui::test]
    fn arming_again_replaces_without_answering(cx: &mut TestAppContext) {
        let (probe, vcx) = open(cx);
        let first = arm_probe(&probe, "x", vcx);
        let second = arm_probe(&probe, "z", vcx);
        probe.read_with(vcx, |p, _| {
            assert_eq!(p.confirm.as_ref().map(|c| *c.payload()), Some("z"));
            assert!(p.cancelled.is_empty(), "the replaced question is not answered");
        });
        assert!(!vcx.update(|window, _| first.is_focused(window)));
        assert!(vcx.update(|window, _| second.is_focused(window)));
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Add `pub mod confirm;` to `crates/geode-tile/src/lib.rs` (first, alphabetical). Run: `cargo test -p geode-tile confirm`
Expected: FAIL to compile — `cannot find trait ConfirmHost`, `arm`, `prompt`…

- [ ] **Step 3: Implement `confirm`**

Prepend to `crates/geode-tile/src/confirm.rs`:

```rust
//! The in-tile y/n confirm. While armed the prompt holds the keyboard: a
//! bare `y` confirms and runs the module's action; any other key, a pointer
//! press anywhere in the tile, or focus leaving the prompt cancels. Every
//! answer blurs the prompt before its handle drops, so the keyboard goes
//! back to the shell root and the shell's restoration path returns it to
//! the tile surface. The module supplies the question and what `y` does;
//! this door owns arming, the answers and the blur.

use gpui::prelude::*;
use gpui::{
    AnyWindowHandle, App, Context, Div, Entity, FocusHandle, KeyDownEvent, SharedString,
    Subscription, Window, div,
};
use gpui_component::Theme;

/// An armed confirm: the module's payload, the question, and the handle
/// the prompt holds the keyboard on. `_blur` is the focus-leaving answer;
/// dropping the confirm drops it, so an answered confirm never also hears
/// its own blur.
pub struct Confirm<P> {
    payload: P,
    prompt: SharedString,
    focus: FocusHandle,
    /// The window the prompt's handle lives in, for [`withdraw`], which runs
    /// where no `Window` is at hand.
    window: AnyWindowHandle,
    _blur: Subscription,
}

impl<P> Confirm<P> {
    pub fn payload(&self) -> &P {
        &self.payload
    }

    pub fn prompt_text(&self) -> &SharedString {
        &self.prompt
    }

    pub fn focus_handle(&self) -> &FocusHandle {
        &self.focus
    }

    pub fn holds_focus(&self, window: &Window) -> bool {
        self.focus.is_focused(window)
    }

    /// Blur, then drop: a focused handle dropped unblurred leaves window
    /// focus on an element no longer painted, and later keys reach no
    /// listener.
    fn disarm(self, window: &mut Window, cx: &mut App) -> P {
        if self.focus.is_focused(window) {
            window.blur(cx);
        }
        self.payload
    }
}

/// The tile a confirm lives on.
pub trait ConfirmHost: Sized + 'static {
    type Payload: 'static;
    /// The tile's one confirm slot.
    fn confirm_slot(&mut self) -> &mut Option<Confirm<Self::Payload>>;
    /// `y`: the prompt is already blurred and dropped.
    fn confirmed(&mut self, payload: Self::Payload, window: &mut Window, cx: &mut Context<Self>);
    /// Any other answer: the prompt is already blurred and dropped.
    fn cancelled(&mut self, payload: Self::Payload, window: &mut Window, cx: &mut Context<Self>);
}

/// Arm a confirm asking `prompt`, focusing its prompt. A confirm already
/// armed is replaced, never stacked, and the replaced one is not answered.
pub fn arm<T: ConfirmHost>(
    host: &mut T,
    payload: T::Payload,
    prompt: impl Into<SharedString>,
    window: &mut Window,
    cx: &mut Context<T>,
) {
    if let Some(old) = host.confirm_slot().take() {
        let _ = old.disarm(window, cx);
    }
    let focus = cx.focus_handle();
    focus.focus(window, cx);
    // Focus leaving the prompt answers no, whatever took it.
    let blur = cx.on_blur(&focus, window, |host: &mut T, window, cx| {
        cancel(host, window, cx);
    });
    *host.confirm_slot() = Some(Confirm {
        payload,
        prompt: prompt.into(),
        focus,
        window: window.window_handle(),
        _blur: blur,
    });
}

/// The prompt's key handler. While armed every key is the confirm's
/// (answers `true`): bare `y` confirms; `n`, escape, a motion, a chord or a
/// shifted `y` cancels. A key that answers the question must not also act
/// on the tile or the shell.
pub fn key<T: ConfirmHost>(
    host: &mut T,
    event: &KeyDownEvent,
    window: &mut Window,
    cx: &mut Context<T>,
) -> bool {
    let Some(armed) = host.confirm_slot().take() else {
        return false;
    };
    let payload = armed.disarm(window, cx);
    let ks = &event.keystroke;
    if ks.key == "y" && !ks.modifiers.modified() {
        host.confirmed(payload, window, cx);
    } else {
        host.cancelled(payload, window, cx);
    }
    true
}

/// Answer no: a pointer press, a blur, or a verb reaching the tile some
/// other way (a palette dispatch). Answers whether a confirm was armed.
pub fn cancel<T: ConfirmHost>(host: &mut T, window: &mut Window, cx: &mut Context<T>) -> bool {
    let Some(armed) = host.confirm_slot().take() else {
        return false;
    };
    let payload = armed.disarm(window, cx);
    host.cancelled(payload, window, cx);
    true
}

/// Withdraw the question where no `Window` is at hand (a delivery that
/// changed what it asked about). The blur subscription drops first, so the
/// deferred blur is not heard as an answer; the handle travels into the
/// deferral, so it is never dropped still focused. The module says why.
pub fn withdraw<T: ConfirmHost>(host: &mut T, cx: &mut Context<T>) -> Option<T::Payload> {
    let Confirm {
        payload,
        focus,
        window,
        _blur,
        ..
    } = host.confirm_slot().take()?;
    drop(_blur);
    cx.defer(move |cx| {
        let _ = window.update(cx, |_, window, cx| {
            if focus.is_focused(window) {
                window.blur(cx);
            }
        });
    });
    Some(payload)
}

/// The prompt: the question on the element that holds the keyboard. Its key
/// listener runs on the focused element, before the shell root's, and stops
/// every key it answers.
pub fn prompt<T: ConfirmHost>(
    confirm: &Confirm<T::Payload>,
    tile: &Entity<T>,
    selector: impl FnOnce() -> String + 'static,
    theme: &Theme,
) -> Div {
    let tile = tile.clone();
    div()
        .track_focus(&confirm.focus)
        .debug_selector(selector)
        .text_color(theme.foreground)
        .child(confirm.prompt.clone())
        .on_key_down(move |event: &KeyDownEvent, window, cx| {
            if tile.update(cx, |t, cx| key(t, event, window, cx)) {
                cx.stop_propagation();
            }
        })
}

/// Cancel on any pointer press in the tile while armed. Capture phase, so it
/// runs before the press reaches what it was aimed at, and it never stops
/// the press. A press on a header or a button moves no focus, so the blur
/// answer alone would leave the question standing behind the click.
pub fn cancel_on_press<E, T>(root: E, armed: bool, tile: &Entity<T>) -> E
where
    E: InteractiveElement + FluentBuilder,
    T: ConfirmHost,
{
    let tile = tile.clone();
    root.when(armed, move |el| {
        el.capture_any_mouse_down(move |_, window, cx| {
            tile.update(cx, |t, cx| cancel(t, window, cx));
        })
    })
}
```

- [ ] **Step 4: Run the confirm tests**

Run: `cargo test -p geode-tile confirm`
Expected: PASS (6 tests).

- [ ] **Step 5: README**

Add to the module table (first row):

```markdown
| `confirm` | The in-tile y/n confirm over a `ConfirmHost` (one `Option<Confirm<P>>` slot, `confirmed`, `cancelled`): `arm`, `key` (bare `y` confirms; every other key cancels and is consumed), `cancel` (pointer, blur, a verb from elsewhere), `withdraw` (no `Window`: drops the blur answer first, defers the blur), `prompt` (the focused question), `cancel_on_press` (capture-phase press on the tile root). Every answer blurs the prompt before its handle drops. |
```

- [ ] **Step 6: Harness entries**

Insert above the last `if [[ -n "$changed_ref" ]]; then`:

```sh
# ---- geode-tile: confirm door ------------------------------------------
#
# Only a bare `y` confirms.
run_mutation "tile confirm: any key confirms" \
  crates/geode-tile/src/confirm.rs \
  '    if ks.key == "y" && !ks.modifiers.modified() {
        host.confirmed(payload, window, cx);' \
  '    if true {
        host.confirmed(payload, window, cx);' \
  geode-tile any_other_key_cancels_and_is_consumed

run_mutation "tile confirm: a modified y confirms" \
  crates/geode-tile/src/confirm.rs \
  '    if ks.key == "y" && !ks.modifiers.modified() {
        host.confirmed(payload, window, cx);' \
  '    if ks.key == "y" {
        host.confirmed(payload, window, cx);' \
  geode-tile any_other_key_cancels_and_is_consumed

# Focus leaving the prompt answers no.
run_mutation "tile confirm: a blur leaves the question standing" \
  crates/geode-tile/src/confirm.rs \
  '    let blur = cx.on_blur(&focus, window, |host: &mut T, window, cx| {
        cancel(host, window, cx);
    });' \
  '    let blur = cx.on_blur(&focus, window, |_: &mut T, _, _| {});' \
  geode-tile a_blur_cancels

# A pointer press in the tile answers no.
run_mutation "tile confirm: a press leaves the question standing" \
  crates/geode-tile/src/confirm.rs \
  '            tile.update(cx, |t, cx| cancel(t, window, cx));' \
  '            let _ = (&tile, window, cx);' \
  geode-tile a_pointer_press_cancels

# Blur, then drop.
run_mutation "tile confirm: the prompt drops still focused" \
  crates/geode-tile/src/confirm.rs \
  '        if self.focus.is_focused(window) {
            window.blur(cx);
        }' \
  '        let _ = (&self.focus, &window, &cx);' \
  geode-tile y_confirms_once_and_gives_the_keyboard_back

# Every key under the question is the question's alone.
run_mutation "tile confirm: an answering key reaches the tile too" \
  crates/geode-tile/src/confirm.rs \
  '            if tile.update(cx, |t, cx| key(t, event, window, cx)) {
                cx.stop_propagation();' \
  '            if tile.update(cx, |t, cx| key(t, event, window, cx)) {' \
  geode-tile any_other_key_cancels_and_is_consumed

```

Then add this entry, which carries the blur subscription into the deferred blur instead of dropping it first:

```sh
# A withdrawal is not an answer: the blur answer drops before the blur.
run_mutation "tile confirm: a withdrawal is heard as a no" \
  crates/geode-tile/src/confirm.rs \
  '    drop(_blur);
    cx.defer(move |cx| {' \
  '    cx.defer(move |cx| {
        let _held = &_blur;' \
  geode-tile withdraw_blurs_without_answering
```

Run `zsh scripts/mutation-check.sh "tile confirm: a withdrawal"`. If it reports SURVIVED (the blur listener runs after the deferred closure has dropped the subscription), delete this one entry — `withdraw_blurs_without_answering` still pins the behavior and the market-data `panel: a rebase under the confirm withdraws it` entry guards the call site — and note the deletion in the commit message.

- [ ] **Step 7: Gates, commit, verify**

```sh
cargo test -p geode-tile
cargo clippy -p geode-tile --all-targets -- -D warnings
cargo fmt --check
zsh scripts/mutation-check.sh --anchors-only
git add crates/geode-tile scripts/mutation-check.sh
git commit -m "feat(tile): confirm door (y/n prompt, blur-then-drop, withdraw)

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
pgrep -f 'mutation-che[c]k'
zsh scripts/mutation-check.sh --build-check "tile confirm:"
zsh scripts/mutation-check.sh "tile confirm:"
```
Expected: no BUILD; every entry `caught` (or the withdrawal entry removed as Step 6 says). Hand-apply each, confirm an assertion failure, restore.

---

### Task 4: Pricer migration (all four doors)

**Files:**
- Modify: `crates/geode-pricer/Cargo.toml`, `src/popup.rs`, `src/paint.rs`, `src/header.rs`, `src/tile.rs`, `README.md`; `docs/current/features.md`; `docs/current/input-and-dialogs.md`; `scripts/mutation-check.sh`

**Interfaces:**
- Consumes: `geode_tile::{popover, menu::{self, ActionRow, Hint, Menu, MenuHost, MenuIds, MenuPick, Row, live_bindings, render_menu}, confirm::{self, Confirm, ConfirmHost}, notice::{self, Notice}}`.
- Produces (crate-internal): `popup::PricerPick { Action(&'static str), View(SharedString) }`; `tile::PendingRemove { sheet: String }`; `PricerTile.menu: Option<Menu<PricerPick>>`, `PricerTile.confirm: Option<Confirm<PendingRemove>>`, `PricerTile.chords: Arc<Vec<Binding>>`; `header::HeaderModel.notice/.save: Option<Notice>`.

- [ ] **Step 1: Dependency**

`crates/geode-pricer/Cargo.toml` `[dependencies]`: add `geode-tile.workspace = true` after `geode-shell.workspace = true`, and add `geode-tile` to the comment above it ("…the tile uses shell hosting, the geode-tile doors, GPUI components…").

- [ ] **Step 2: Write the failing tests (live hint, rebind, keymap reload)**

In `crates/geode-pricer/src/tile.rs` `mod tests`, add near `menu_rows`:

```rust
    /// The keymap the shell would publish: the pricer fragment over the
    /// builtin actions, plus `user` as the user layer.
    fn install_chords(vcx: &mut VisualTestContext, user: Option<&str>) {
        use geode_core::config::{Layer, LayerDoc};
        use geode_shell::actions::{ActionDef, ActionRegistry};
        let mut registry = ActionRegistry::default();
        geode_shell::defaults::register_builtin_actions(&mut registry);
        for (id, title) in crate::content::ACTIONS {
            registry
                .register(ActionDef {
                    id: ActionId(id.to_string()),
                    title: title.to_string(),
                    category: "Pricer".into(),
                })
                .unwrap();
        }
        let mut docs = vec![
            geode_shell::keymap::fragments::fragment_doc("pricer", crate::content::DEFAULT_KEYMAP)
                .unwrap(),
        ];
        if let Some(text) = user {
            docs.push(LayerDoc {
                layer: Layer::User,
                name: "keymap".into(),
                file: "user/keymap.toml".into(),
                table: text.parse().unwrap(),
            });
        }
        let (keymap, diags) = geode_shell::keymap::build_keymap(
            &docs,
            geode_shell::defaults::default_mod(),
            &registry,
        );
        assert!(diags.is_empty(), "{diags:?}");
        vcx.update(|_, cx| {
            cx.set_global(geode_shell::tips::Chords(std::sync::Arc::new(
                keymap.bindings().to_vec(),
            )))
        });
        vcx.run_until_parked();
    }

    const GROUP_REBOUND: &str = "[[bindings]]\ncontext = \"pricer && mode == normal\"\n[bindings.keys]\n\"g p\" = \"none\"\n\"g shift+p\" = \"pricer::group\"\n";

    fn group_lane(h: &Harness, vcx: &VisualTestContext) -> String {
        menu_rows(h, vcx)
            .into_iter()
            .find(|r| r.starts_with("Group into package |"))
            .expect("the group row")
    }

    #[gpui::test]
    fn a_menu_hint_follows_a_user_rebind(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_seeded(cx, &BOOK);
        install_chords(&mut vcx, Some(GROUP_REBOUND));
        h.dispatch(&mut vcx, "menu", None);
        assert_eq!(group_lane(&h, &vcx), "Group into package | g shift+p");
    }

    #[gpui::test]
    fn an_open_menu_follows_a_keymap_reload(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_seeded(cx, &BOOK);
        install_chords(&mut vcx, None);
        h.dispatch(&mut vcx, "menu", None);
        assert_eq!(group_lane(&h, &vcx), "Group into package | g p");
        install_chords(&mut vcx, Some(GROUP_REBOUND));
        assert_eq!(group_lane(&h, &vcx), "Group into package | g shift+p");
    }
```

and replace `menu_rows` (it reads the door's rows now; `render_binding` is the keymap spelling):

```rust
    fn menu_rows(h: &Harness, vcx: &VisualTestContext) -> Vec<String> {
        use geode_tile::menu::{Row, Trailing};
        h.tile.read_with(vcx, |t, _| {
            t.menu
                .as_ref()
                .expect("the menu is open")
                .rows()
                .iter()
                .map(|r| match r {
                    Row::Action(a) => match a.pick() {
                        PricerPick::View(name) => format!(
                            "{} {name}",
                            if a.checked() == Some(true) { "✓" } else { " " }
                        ),
                        PricerPick::Action(_) => match (a.reason(), a.trailing()) {
                            (Some(why), _) => format!("{} | ({why})", a.title()),
                            (None, Trailing::Keys(k)) => format!(
                                "{} | {}",
                                a.title(),
                                geode_shell::palette::render_binding(k)
                            ),
                            (None, Trailing::Text(t)) => format!("{} | {t}", a.title()),
                            (None, Trailing::None) => format!("{} | ", a.title()),
                        },
                    },
                    Row::Separator => "—".into(),
                    Row::Section(s) => format!("[{s}]"),
                })
                .collect()
        })
    }
```

Add `install_chords(&mut vcx, None);` as the first line after `open_seeded` in `the_menu_groups_its_rows_names_keys_and_says_why_a_row_is_disabled` (its expected strings are unchanged: the shipped keymap binds `g p`, `d d`, and nothing binds `pricer::price`, so the verb `:price` shows).

- [ ] **Step 3: Run to verify they fail**

Run: `cargo test -p geode-pricer menu`
Expected: FAIL to compile (`PricerPick` unknown, `rows()` not a method of `popup::Menu`).

- [ ] **Step 4: Rewrite `popup.rs`**

Replace `crates/geode-pricer/src/popup.rs` whole with:

```rust
//! Cell choice typeahead, the entry bar's completion list, and the pick type
//! behind the tile's action menu. Popup geometry and layering, and the menu
//! itself, are `geode_tile`'s; what these lists hold is the pricer's.

use crate::core::complete::Completion;
use crate::tile::PricerTile;
use geode_shell::choice::ChoiceList;
use geode_shell::shell::scale;
use geode_tile::menu::MenuPick;
use geode_tile::popover::{self, ROW_HEIGHT, ROW_INSET};
use gpui::prelude::*;
use gpui::{Anchor, App, Entity, MouseButton, SharedString, div};
use gpui_component::{ActiveTheme as _, h_flex};
```

then keep, unchanged, `ChoicePaint`, `choice_paint` and `NO_UNDERLYINGS`; keep `render_choice` and `render_entry_list` with these three edits each:
- `popover_surface(cx)` → `popover::surface(cx)`;
- the empty-row `div()…child("no option matches")` block → `popover::empty_row(theme, "no option matches")`, and in `render_entry_list` the `NO_UNDERLYINGS` block → `popover::empty_row(theme, NO_UNDERLYINGS).debug_selector(|| "pricer-entry-none".into())`;
- the trailing `deferred(anchored()…).with_priority(1)` → `popover::anchor_popup(list, Anchor::TopLeft)` (inside `Some(..)` for the entry list).

Delete `ROW_HEIGHT`, `ROW_INSET`, `MIN_WIDTH`, `TICK_SLOT`, `SNAP_MARGIN`, `popover_surface`, `MenuItem`, `Menu`, `step`, `snap`, `MenuRowPaint`, `menu_row_paint`, `render_menu` and the whole `mod tests` (their coverage now lives in `geode_tile::menu` tests; the harness entries are re-aimed in Step 11). Append:

```rust
/// What a pricer menu row does when picked: dispatch one of the tile's
/// actions, or show the sheet through a view.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum PricerPick {
    Action(&'static str),
    View(SharedString),
}

impl MenuPick for PricerPick {
    fn element_name(&self) -> SharedString {
        match self {
            PricerPick::Action(id) => SharedString::new_static(id),
            PricerPick::View(name) => format!("view:{name}").into(),
        }
    }
}
```

- [ ] **Step 5: Drop the menu colors from `paint.rs`**

In `crates/geode-pricer/src/paint.rs`: delete the four `menu_*` fields and their doc comment from `Paints`, their four lines in `derive`, the `let (popover, active) = Self::menu_grounds(theme);` line, the `menu_grounds` fn, and in `every_pricer_paint_is_readable_on_every_bundled_theme` the `let (popover, active) = …` line and the four `("menu …", …)` tuples with their comment. Change the sweep's floor assertion from `checked >= 22 * 40` to `checked >= 18 * 40` (four checks per theme fewer). Update the module doc's second paragraph to drop "Menu text is adjusted against the popover or enabled-row highlight." (menu colors live in `geode_tile::menu::MenuPaint`).

- [ ] **Step 6: Menu in `tile.rs`**

Imports: replace `use crate::popup::{Menu, MenuItem, choice_paint, render_menu};` with

```rust
use crate::popup::{PricerPick, choice_paint};
use geode_shell::keymap::Binding;
use geode_tile::confirm::{self, Confirm, ConfirmHost};
use geode_tile::menu::{self, ActionRow, Hint, Menu, MenuHost, MenuIds, Row};
use std::sync::Arc;
```

(keep any existing `Arc` import single). Field: `menu: Option<Menu>` → `menu: Option<Menu<PricerPick>>`. Add a field next to it:

```rust
    /// The keymap as last published, for the menu's key hints: read at
    /// construction and on every `Chords` publish, so a chrome rebuild (which
    /// has no `App`) resolves hints against the live keymap.
    chords: Arc<Vec<Binding>>,
```

initialized in `new` with `chords: menu::live_bindings(cx),`. Beside the other `observe_global` calls in `new` (after the `AppClock` one) add:

```rust
        // A keymap reload re-resolves an open menu's hints at once.
        cx.observe_global::<geode_shell::tips::Chords>(|this, cx| {
            this.chords = menu::live_bindings(cx);
            if let Some(m) = this.menu.as_mut() {
                m.rehint(&this.chords);
                cx.notify();
            }
        })
        .detach();
```

Replace `menu_items` with:

```rust
    /// Action groups, then the available views. Titles are the palette's
    /// (`content::action_title`); hints are the actions' live chords (a
    /// `:price` verb when unbound); a disabled action carries its reason.
    fn menu_items(&self) -> Vec<Row<PricerPick>> {
        let row = self.cursor_sheet_row();
        let root_line =
            row.is_some_and(|r| self.sheet.is_line(r) && self.sheet.parent(r).is_none());
        let packaged =
            row.is_some_and(|r| self.sheet.is_package(r) || self.sheet.parent(r).is_some());
        let action = |id: &'static str, hint: Hint, enabled: Result<(), &'static str>| {
            Row::Action(
                ActionRow::new(PricerPick::Action(id), crate::content::action_title(id))
                    .hint(hint)
                    .enabled(enabled.map_err(SharedString::new_static)),
            )
        };
        let mut items = vec![
            action("pricer::price", Hint::chord_or_verb("pricer::price", ":price"), Ok(())),
            Row::Separator,
            action(
                "pricer::group",
                Hint::chord("pricer::group"),
                if root_line {
                    Ok(())
                } else {
                    Err("group needs a top-level line")
                },
            ),
            action(
                "pricer::ungroup",
                Hint::chord("pricer::ungroup"),
                if packaged {
                    Ok(())
                } else {
                    Err("not in a package")
                },
            ),
            Row::Separator,
            action(
                "pricer::undo",
                Hint::chord("pricer::undo"),
                if self.undo.can_undo() {
                    Ok(())
                } else {
                    Err("nothing to undo")
                },
            ),
            action(
                "pricer::redo",
                Hint::chord("pricer::redo"),
                if self.undo.can_redo() {
                    Ok(())
                } else {
                    Err("nothing to redo")
                },
            ),
            Row::Separator,
            action(
                "pricer::delete",
                Hint::chord("pricer::delete"),
                if row.is_some() { Ok(()) } else { Err("no row") },
            ),
        ];
        let views = self.shared.views.borrow();
        if !views.is_empty() {
            items.push(Row::Separator);
            items.push(Row::Section(SharedString::new_static("View")));
            items.extend(views.names().map(|name| {
                let label: SharedString = name.to_string().into();
                Row::Action(
                    ActionRow::new(PricerPick::View(label.clone()), label)
                        .checked(name == self.sheet.view),
                )
            }));
        }
        items
    }
```

`toggle_menu`: `None => Some(Menu::new(self.menu_items(), &self.chords)),`.

Delete the inherent `menu_hover` and `menu_pick` and add (below `close_menu`):

```rust
impl MenuHost for PricerTile {
    /// A disabled row says why and keeps the menu open; an enabled one
    /// closes it and dispatches through the same door a key would.
    fn menu_pick(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(picked) = self.menu.as_ref().and_then(|m| m.pick(index)) else {
            return;
        };
        match picked {
            Err(why) => {
                self.footer = Some(why);
                self.rebuild_chrome();
                cx.notify();
            }
            Ok(PricerPick::Action(id)) => {
                self.menu = None;
                self.dispatch(&ActionId(id.to_string()), None, window, cx);
            }
            Ok(PricerPick::View(name)) => {
                self.menu = None;
                if let Err(why) = self.set_view(&name, cx) {
                    self.footer = Some(why.into());
                }
                self.rebuild_chrome();
                cx.notify();
            }
        }
    }

    /// Change-only: gpui fires this on every pointer move over a row.
    fn menu_hover(&mut self, index: usize, cx: &mut Context<Self>) {
        if self.menu.as_mut().is_some_and(|m| m.highlight(index)) {
            cx.notify();
        }
    }
}
```

In `dispatch`, `"menu_down" | "menu_up"`: `m.highlighted = crate::popup::step(&m.items, m.highlighted, delta);` → `m.step(delta);`. `"menu_pick"`: `let at = self.menu.as_ref().and_then(|m| m.highlighted());` (the `if let Some(at)` body unchanged).

`rebuild_chrome`, the open-menu block becomes:

```rust
        // Recompute an open menu after load, reload, or delivery changes its
        // contents; the door snaps its highlight onto an action row.
        if self.menu.is_some() {
            let items = self.menu_items();
            if let Some(m) = self.menu.as_mut() {
                m.replace_rows(items, &self.chords);
            }
        }
```

`render`: delete `let paints = self.table.read(cx).delegate().paints;` if nothing else in `render` reads `paints` (the compiler's unused-variable warning decides), and replace `render_menu(m, &paints, &tile, cx)` with:

```rust
menu::render_menu(
    m,
    &MenuIds::new("pricer-menu", "pricer-menu-row"),
    gpui::Anchor::TopRight,
    &tile,
    |t: &mut PricerTile, _, cx| t.close_menu(cx),
    cx,
)
```

Tests in `tile.rs` that read the old model — update each (compile errors list them):
- `.map(|m| m.highlighted)` → `.and_then(|m| m.highlighted())`; `assert_eq!(at, Some(2), …)` etc. are unchanged in value.
- `a_pointer_over_a_disabled_menu_row_lands_without_a_fill`: read `let row = m.rows()[m.highlighted().unwrap()].action().unwrap(); let enabled = row.is_enabled();` and compute the paint with `let p = geode_tile::menu::MenuPaint::derive(cx.theme()); let paint = geode_tile::menu::row_paint(&p, true, enabled);`; assert `paint.fill == None` and `paint.text == p.muted` (drop `paints`).
- The reload test near line 6650 (`views`, `highlighted`): iterate `m.rows()` with `Row::Action(a) if matches!(a.pick(), PricerPick::View(_))` → `format!("{} {}", a.title(), a.checked() == Some(true))`, and `m.highlighted()` → expected `Some(11)`.

- [ ] **Step 7: Confirm in `tile.rs` and `header.rs`**

Replace the `PendingRemove` struct and its doc with:

```rust
/// What an armed `:rm` asks to remove. The prompt, its focus and its blur
/// answer are the `geode_tile::confirm` door's.
pub(crate) struct PendingRemove {
    sheet: String,
}
```

Field `confirm: Option<PendingRemove>` → `confirm: Option<Confirm<PendingRemove>>` (doc unchanged). In `arm_remove`, replace everything from `let focus = cx.focus_handle();` to the end of the `self.confirm = Some(PendingRemove { … });` statement with:

```rust
        let prompt = format!("remove sheet '{name}' and all its history? (y/n)");
        confirm::arm(self, PendingRemove { sheet: name }, prompt, window, cx);
```

Delete `confirm_key`, `cancel_remove_on_pointer`, `disarm_remove` and `cancel_remove`. Change `submit_remove` to take the payload and no window:

```rust
    /// `y`: forget the sheet. Whether it went reaches the store through
    /// `PricerFactory::forget_answered`, and a failure is painted here.
    fn submit_remove(&mut self, pending: PendingRemove, cx: &mut Context<Self>) {
```

deleting its first three lines (`let Some(pending) = self.disarm_remove(window, cx) else { return; };`); the rest of the body is unchanged (it still reads `pending.sheet`). Add:

```rust
impl ConfirmHost for PricerTile {
    type Payload = PendingRemove;

    fn confirm_slot(&mut self) -> &mut Option<Confirm<PendingRemove>> {
        &mut self.confirm
    }

    fn confirmed(&mut self, pending: PendingRemove, _: &mut Window, cx: &mut Context<Self>) {
        self.submit_remove(pending, cx);
    }

    fn cancelled(&mut self, _: PendingRemove, _: &mut Window, cx: &mut Context<Self>) {
        self.footer = Some(NOT_REMOVED.into());
        self.rebuild_chrome();
        cx.notify();
    }
}
```

`dispatch`: `if self.confirm.is_some() { self.cancel_remove(window, cx); }` → `confirm::cancel(self, window, cx);`. `holds_focus`: `.is_some_and(|c| c.focus.is_focused(window))` → `.is_some_and(|c| c.holds_focus(window))`. `rebuild_chrome`: `prompt: self.confirm.as_ref().map(|c| c.prompt.clone())` → `.map(|c| c.prompt_text().clone())`. `render`: `confirm: self.confirm.as_ref().map(|c| &c.focus)` → `confirm: self.confirm.as_ref()`; replace the root's `.when(self.confirm.is_some(), |el| { el.capture_any_mouse_down(…) })` and the `cancel_tile` binding with wrapping the root: `confirm::cancel_on_press(v_flex()…/* existing chain without the .when */, self.confirm.is_some(), &tile)`.

Tests: `c.prompt.to_string()` → `c.prompt_text().to_string()`; `.focus.clone()` → `.focus_handle().clone()`.

`header.rs`: imports add `use geode_tile::confirm::{self, Confirm};` and `use geode_tile::notice::{self, Notice};`, and `use crate::tile::PendingRemove;`. Delete `NoticeTone`. `HeaderModel`: `pub notice: Option<Notice>,` (drop `notice_tone`), `pub save: Option<Notice>,`. `HeaderChrome.confirm: Option<&'a Confirm<PendingRemove>>` with doc "The armed `:rm` confirm: its prompt is painted through the confirm door." In `prepare`:

```rust
    let notice = match i.notice {
        Some(n) if n.as_ref() == LOADING => Some(Notice::status(n)),
        Some(n) => Some(Notice::warning(n)),
        None => i.settings.pricer_missing.then(|| {
            Notice::danger(format!(
                "pricer '{}' is not built into this binary; set [pricing] adapter and restart",
                i.settings.pricer
            ))
        }),
    };
```

and in the struct literal `notice,` and `save: i.save.map(Notice::warning),`. `texts()`: `self.save.iter().map(|s| s.text().to_string())` and the same for `notice`. `render`: delete `notice_colour`; replace the save, notice and prompt children with:

```rust
        .when_some(h.save.as_ref(), |el, n| {
            el.child(notice::render(n, theme).debug_selector(|| "pricer-save-notice".into()))
        })
        .when_some(h.notice.as_ref(), |el, n| {
            el.child(notice::render(n, theme).debug_selector(|| "pricer-notice".into()))
        })
        .when_some(h.prompt.as_ref().and(c.confirm), |el, pending| {
            el.child(confirm::prompt(
                pending,
                c.tile,
                move || format!("pricer-remove-confirm-{tile_id}"),
                theme,
            ))
        })
```

Header tests: `h.notice.as_deref()` → `h.notice.as_ref().map(|n| n.text().as_ref())`; `h.notice_tone` → `h.notice.as_ref().map(Notice::tone)` compared with `Some(notice::Tone::Danger)` / `Status` / `Warning`. Harness readers in `tile.rs`: `.map(|n| n.to_string())` → `.map(|n| n.text().to_string())` for `notice` and `save_notice`.

- [ ] **Step 8: Run the pricer suite**

Run: `cargo test -p geode-pricer`
Expected: PASS, including `a_menu_hint_follows_a_user_rebind`, `an_open_menu_follows_a_keymap_reload`, every `rm` test, `the_menu_groups_its_rows_names_keys_and_says_why_a_row_is_disabled`, `a_reload_under_an_open_menu_relists_its_views`. Then `cargo test -p geode-app a_key_answering_the_rm_confirm_reaches_nothing_else` → PASS.

- [ ] **Step 9: Docs**

`crates/geode-pricer/README.md`: module-map `popup` row → "The typeahead, the entry bar's completion list, and `PricerPick` (what a menu row does). The menu, popup geometry, the `:rm` confirm and the header notices paint through `geode-tile`." `header` row → "The prepared header row (notices as `geode_tile::notice::Notice`) and footer." In the paint bullet (≈ line 266) drop "and action-menu text colors … menu text against popover and enabled highlight backgrounds"; menu colors are `geode_tile::menu::MenuPaint`'s. In the `:rm` bullet (≈ line 226) say the prompt is `geode_tile::confirm`'s. Replace the Known-limitations bullet "The action menu's key hints are the default bindings …" with nothing if market-data is already migrated; at this task it becomes: "- The market-data action list's key hints are still the default bindings; a user rebind is not reflected there."

`docs/current/features.md` (pricer section, ≈ line 828): "Key hints show default bindings and do not reflect rebindings." → "Key hints are the actions' live chords (`:price` when the keymap binds none) and follow a keymap reload while the menu is open."

`docs/current/input-and-dialogs.md` (≈ line 228): "…does not change the empty-state hint's `ctrl+k` or a module menu's hint; the palette, keybinding rows, tooltips and the timeseries footer and menu read the live keymap." → "…does not change the empty-state hint's `ctrl+k` or the market-data menu's hints; the palette, keybinding rows, tooltips, the timeseries footer, and the timeseries and pricer menus read the live keymap."

- [ ] **Step 10: Re-aim the pricer harness entries**

Replace each named entry's `run_mutation` block:

```sh
run_mutation "pricer popup: menu steps land on pickable rows only" \
  crates/geode-tile/src/menu/mod.rs \
  '            (at + 1..rows.len()).find(|&i| rows[i].lands())' \
  '            (at + 1..rows.len()).next()' \
  geode-pricer the_menu_groups_its_rows_names_keys_and_says_why_a_row_is_disabled
```
```sh
run_mutation "pricer popup: menu steps skip disabled rows" \
  crates/geode-tile/src/menu/mod.rs \
  '            (at + 1..rows.len()).find(|&i| rows[i].lands())' \
  '            (at + 1..rows.len()).find(|&i| rows[i].is_action())' \
  geode-pricer the_menu_groups_its_rows_names_keys_and_says_why_a_row_is_disabled
```
```sh
run_mutation "pricer popup: a disabled menu row takes no fill" \
  crates/geode-tile/src/menu/paint.rs \
  '        (_, false) => RowPaint {
            fill: None,' \
  '        (_, false) => RowPaint {
            fill: Some(p.active_fill),' \
  geode-pricer a_pointer_over_a_disabled_menu_row_lands_without_a_fill
```
```sh
run_mutation "pricer tile: an open menu re-checks its rows on a rebuild" \
  crates/geode-pricer/src/tile.rs \
  '                m.replace_rows(items, &self.chords);' \
  '                let _ = items;' \
  geode-pricer a_reload_under_an_open_menu_relists_its_views
```
```sh
run_mutation "pricer tile: a re-checked menu clamps its highlight" \
  crates/geode-tile/src/menu/mod.rs \
  '        self.highlighted = snap(&self.rows, self.highlighted);' \
  '' \
  geode-pricer a_reload_under_an_open_menu_relists_its_views
```
```sh
run_mutation "pricer rm: any key confirms" \
  crates/geode-tile/src/confirm.rs \
  '    if ks.key == "y" && !ks.modifiers.modified() {
        host.confirmed(payload, window, cx);' \
  '    if true {
        host.confirmed(payload, window, cx);' \
  geode-pricer any_other_key_cancels_the_rm_confirm_and_is_consumed
```
```sh
run_mutation "pricer rm: a modified y confirms" \
  crates/geode-tile/src/confirm.rs \
  '    if ks.key == "y" && !ks.modifiers.modified() {
        host.confirmed(payload, window, cx);' \
  '    if ks.key == "y" {
        host.confirmed(payload, window, cx);' \
  geode-pricer any_other_key_cancels_the_rm_confirm_and_is_consumed
```
```sh
run_mutation "pricer rm: focus leaving leaves the question standing" \
  crates/geode-tile/src/confirm.rs \
  '    let blur = cx.on_blur(&focus, window, |host: &mut T, window, cx| {
        cancel(host, window, cx);
    });' \
  '    let blur = cx.on_blur(&focus, window, |_: &mut T, _, _| {});' \
  geode-pricer focus_leaving_or_a_pointer_press_cancels_the_rm_confirm
```
```sh
run_mutation "pricer rm: a pointer press leaves the question standing" \
  crates/geode-tile/src/confirm.rs \
  '            tile.update(cx, |t, cx| cancel(t, window, cx));' \
  '            let _ = (&tile, window, cx);' \
  geode-pricer focus_leaving_or_a_pointer_press_cancels_the_rm_confirm
```
```sh
run_mutation "pricer rm: the confirm drops still focused" \
  crates/geode-tile/src/confirm.rs \
  '        if self.focus.is_focused(window) {
            window.blur(cx);
        }' \
  '        let _ = (&self.focus, &window, &cx);' \
  geode-pricer any_other_key_cancels_the_rm_confirm_and_is_consumed
```
```sh
run_mutation "pricer rm: a key under the confirm reaches the tile too" \
  crates/geode-tile/src/confirm.rs \
  '            if tile.update(cx, |t, cx| key(t, event, window, cx)) {
                cx.stop_propagation();' \
  '            if tile.update(cx, |t, cx| key(t, event, window, cx)) {' \
  geode-app a_key_answering_the_rm_confirm_reaches_nothing_else
```

Add one new entry for the deliberate change:

```sh
# The pricer menu's hints are the live keymap's: a user rebind shows.
run_mutation "pricer menu: a rebind does not reach the menu" \
  crates/geode-pricer/src/tile.rs \
  '            this.chords = menu::live_bindings(cx);' \
  '            let _ = menu::live_bindings(cx);' \
  geode-pricer an_open_menu_follows_a_keymap_reload
```

- [ ] **Step 11: Gates**

```sh
cargo test -p geode-pricer
cargo test -p geode-tile
cargo test -p geode-app a_key_answering_the_rm_confirm_reaches_nothing_else
cargo clippy -p geode-pricer --all-targets -- -D warnings
cargo fmt --check
zsh scripts/mutation-check.sh --anchors-only
```

- [ ] **Step 12: Commit, verify the re-aimed and new entries**

```bash
git add crates/geode-pricer docs/current/features.md docs/current/input-and-dialogs.md scripts/mutation-check.sh Cargo.lock
git commit -m "refactor(pricer): menu, confirm, popups and notices through geode-tile

Menu key hints now follow the live keymap (the_menu_groups_its_rows_names_keys_and_says_why_a_row_is_disabled
installs the shipped keymap; new: a_menu_hint_follows_a_user_rebind,
an_open_menu_follows_a_keymap_reload). Menu row colors are the door's
floored MenuPaint; the pricer's own menu paints and their sweep rows go.

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
pgrep -f 'mutation-che[c]k'
zsh scripts/mutation-check.sh --build-check "pricer popup:"
zsh scripts/mutation-check.sh --build-check "pricer rm:"
zsh scripts/mutation-check.sh --build-check "pricer tile: a"
zsh scripts/mutation-check.sh --build-check "pricer menu:"
zsh scripts/mutation-check.sh "pricer popup:"
zsh scripts/mutation-check.sh "pricer rm: a"
zsh scripts/mutation-check.sh "pricer rm: the confirm drops"
zsh scripts/mutation-check.sh "pricer rm: focus leaving"
zsh scripts/mutation-check.sh "pricer rm: any key"
zsh scripts/mutation-check.sh "pricer tile: an open menu"
zsh scripts/mutation-check.sh "pricer tile: a re-checked"
zsh scripts/mutation-check.sh "pricer menu:"
```
Expected: no BUILD; each re-aimed/new entry `caught`. Hand-apply each re-aimed replacement once and confirm the named test fails on an assertion; restore.

---

### Task 5: Market-data migration (popover, menu, confirm, notices)

`PanelSpec`, the header layout and the grid model are not touched.

**Files:**
- Modify: `crates/geode-marketdata/Cargo.toml`, `src/core/menu.rs`, `src/popup.rs`, `src/header.rs`, `src/tile.rs`, `README.md`; `crates/geode-pricer/README.md` (limitation line); `docs/current/input-and-dialogs.md`; `scripts/mutation-check.sh`

**Interfaces:**
- Consumes: Tasks 1–3 doors.
- Produces: `core::menu::rows(&MenuInputs, Clock) -> Vec<Row<ActionId>>`; `popup::Popup::Menu(Menu<ActionId>)`; `tile::PendingUpload { target, rows, draft }` (payload only, `pub(crate)`); `MarketDataTile.pending_upload: Option<Confirm<PendingUpload>>`, `.menu_ids: MenuIds`, `.chords: Arc<Vec<Binding>>`; `header::HeaderModel.notice/.upload_error: Option<Notice>`.

- [ ] **Step 1: Dependency**

`crates/geode-marketdata/Cargo.toml`: add `geode-tile.workspace = true` after `geode-shell.workspace = true`; extend the comment: "`geode-tile` for the action menu, popups, the upload confirm and notices".

- [ ] **Step 2: Write the failing tests**

In `crates/geode-marketdata/src/tile.rs` tests, change `install_fragment_chords` to take a user layer and add the tests:

```rust
    /// Install Chords from ACTIONS, the fragment, and `user` as the user
    /// layer, so chord lookups use the keymap the shell would publish.
    fn install_chords(vcx: &mut gpui::VisualTestContext, user: Option<&str>) {
        let mut registry = geode_shell::actions::ActionRegistry::default();
        for (id, title) in crate::content::ACTIONS {
            registry
                .register(geode_shell::actions::ActionDef {
                    id: ActionId((*id).to_string()),
                    title: (*title).to_string(),
                    category: "Market data".to_string(),
                })
                .expect("no duplicate ids");
        }
        let mut docs = vec![
            geode_shell::keymap::fragments::fragment_doc(CVI.kind, crate::content::DEFAULT_KEYMAP)
                .expect("the fragment parses"),
        ];
        if let Some(text) = user {
            docs.push(geode_core::config::LayerDoc {
                layer: geode_core::config::Layer::User,
                name: "keymap".into(),
                file: "user/keymap.toml".into(),
                table: text.parse().unwrap(),
            });
        }
        let (keymap, diags) = geode_shell::keymap::build_keymap(
            &docs,
            geode_shell::defaults::default_mod(),
            &registry,
        );
        assert!(diags.is_empty(), "{diags:?}");
        vcx.update(|_window, cx| {
            cx.set_global(geode_shell::tips::Chords(Arc::new(keymap.bindings().to_vec())));
        });
        vcx.run_until_parked();
    }

    fn install_fragment_chords(vcx: &mut gpui::VisualTestContext) {
        install_chords(vcx, None);
    }

    const LOAD_REBOUND: &str = "[[bindings]]\ncontext = \"marketdata && mode == normal\"\n[bindings.keys]\n\"u\" = \"none\"\n\"shift+u\" = \"marketdata::load_underlying\"\n";

    /// The open menu's `(title, lane)` for `title`, the lane in keymap
    /// spelling or as text.
    fn menu_lane(h: &Harness, vcx: &gpui::VisualTestContext, title: &str) -> String {
        use geode_tile::menu::{Row, Trailing};
        h.tile.read_with(vcx, |t, _| match &t.popup {
            Some(Popup::Menu(m)) => m
                .rows()
                .iter()
                .find_map(|r| match r {
                    Row::Action(a) if a.title().as_ref() == title => Some(match a.trailing() {
                        Trailing::Keys(k) => geode_shell::palette::render_binding(k),
                        Trailing::Text(t) => t.to_string(),
                        Trailing::None => String::new(),
                    }),
                    _ => None,
                })
                .expect("the row"),
            _ => panic!("the menu is open"),
        })
    }

    #[gpui::test]
    fn a_menu_hint_follows_a_user_rebind(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.with_document(&mut vcx);
        install_chords(&mut vcx, Some(LOAD_REBOUND));
        h.dispatch(&mut vcx, "menu", None);
        assert_eq!(menu_lane(&h, &vcx, "Load underlying…"), "shift+u");
        assert_eq!(
            menu_lane(&h, &vcx, "Revert edits"),
            "nothing to revert",
            "a disabled row still shows its reason"
        );
    }

    #[gpui::test]
    fn an_open_menu_follows_a_keymap_reload(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.with_document(&mut vcx);
        install_chords(&mut vcx, None);
        h.dispatch(&mut vcx, "menu", None);
        assert_eq!(menu_lane(&h, &vcx, "Load underlying…"), "u");
        install_chords(&mut vcx, Some(LOAD_REBOUND));
        assert_eq!(menu_lane(&h, &vcx, "Load underlying…"), "shift+u");
    }
```

(`Popup` is already imported in the tests module via `super::*`; `t.popup` is the tile's field. If the harness type is spelled differently, use the one `open` returns.)

- [ ] **Step 3: Run to verify they fail**

Run: `cargo test -p geode-marketdata menu_hint`
Expected: FAIL to compile (`m.rows()` — `MenuState` has no method `rows`).

- [ ] **Step 4: `core/menu.rs` on door rows**

Replace the imports and everything above `#[cfg(test)]` except `MenuInputs` with:

```rust
use crate::core::draft::{DraftBadge, UpdatePolicy, local_hhmm};
use crate::core::spec::KindAction;
use geode_core::clock::Clock;
use geode_shell::actions::ActionId;
use geode_tile::menu::{ActionRow, Hint, Row};
use gpui::SharedString;

// `MenuInputs` unchanged.

fn action(
    id: &'static str,
    title: impl Into<SharedString>,
    hint: Hint,
    enabled: Result<(), &'static str>,
) -> Row<ActionId> {
    Row::Action(
        ActionRow::new(ActionId(id.to_string()), title)
            .hint(hint)
            .enabled(enabled.map_err(SharedString::new_static)),
    )
}

/// The three "On new document" rows, in [`UpdatePolicy::ALL`]'s order,
/// ticked where `policy` matches: a choice group, exactly one in force.
fn policy_rows(policy: UpdatePolicy) -> impl Iterator<Item = Row<ActionId>> {
    UpdatePolicy::ALL.into_iter().map(move |p| {
        let (id, title) = match p {
            UpdatePolicy::Hold => ("marketdata::auto_hold", "hold edits"),
            UpdatePolicy::Rebase => ("marketdata::auto_rebase", "rebase edits"),
            UpdatePolicy::Replace => ("marketdata::auto_replace", "replace edits"),
        };
        Row::Action(
            ActionRow::new(ActionId(id.to_string()), title)
                .checked(p == policy),
        )
    })
}
```

In `rows`, change the return type to `Vec<Row<ActionId>>` and the hint arguments: `"u"` → `Hint::chord("marketdata::load_underlying")`, `":upload"` → `Hint::chord_or_verb("marketdata::upload", ":upload")`, `":rebase"` → `Hint::chord_or_verb("marketdata::rebase", ":rebase")`, `":revert"` → `Hint::chord_or_verb("marketdata::revert", ":revert")`, the kind-action `""` → `Hint::None`; `MenuRow::Separator` → `Row::Separator`; `MenuRow::Section(x.into())` → `Row::Section(x.into())`. Delete `first_enabled`, `lands`, `step` (the door's). Tests in this file: add `use geode_tile::menu::{first_enabled, step};`; rewrite the helpers:

```rust
    fn checked(rows: &[Row<ActionId>]) -> Vec<(String, Option<bool>)> {
        rows.iter()
            .filter_map(|r| r.action().map(|a| (a.title().to_string(), a.checked())))
            .collect()
    }
    fn titles(rows: &[Row<ActionId>]) -> Vec<String> {
        rows.iter()
            .map(|r| match r {
                Row::Action(a) => a.title().to_string(),
                Row::Separator => "—".into(),
                Row::Section(s) => format!("[{s}]"),
            })
            .collect()
    }
    fn enabled(rows: &[Row<ActionId>], title: &str) -> Result<(), String> {
        rows.iter()
            .find_map(|r| r.action().filter(|a| a.title().as_ref() == title))
            .map(|a| a.reason().map_or(Ok(()), |r| Err(r.to_string())))
            .unwrap()
    }
```

and in the assertions: `Err("…")` → `Err("…".into())`; `first_enabled(&rows)` → `Some(0)` expectations; `step(&rows, a, d)` → `step(&rows, Some(a), d)` with `Some(..)` expected values.

- [ ] **Step 5: `popup.rs`**

Imports: drop `use crate::core::menu::MenuRow;`, `Anchor`/`AnchoredPositionMode`/`anchored`/`deferred`/`px`/`Div`/`h_flex`-if-unused; add `use geode_tile::popover::{self, ROW_HEIGHT, ROW_INSET};` and `use geode_tile::menu::Menu;` and `use geode_shell::actions::ActionId;`. Delete the three constants, `popover_surface`, `MenuState` and `render_menu`. `Popup::Menu(MenuState)` → `Popup::Menu(Menu<ActionId>)` (doc: "the action list: the door's menu over action ids"). In `render_picker` and `render_choice`: `popover_surface(cx)` → `popover::surface(cx)`; the "no underlyings known" and "no option matches" rows → `popover::empty_row(theme, "no underlyings known")` / `popover::empty_row(theme, "no option matches")`; the trailing `deferred(anchored()…)` → `popover::anchor_popup(list, Anchor::TopRight)` (picker) and `popover::anchor_popup(list, Anchor::TopLeft)` (choice). Update the module doc's first sentence: "Tile-owned underlying picker and cell-choice popups; the action menu is `geode_tile::menu`'s."

- [ ] **Step 6: Menu in `tile.rs`**

Imports: `use crate::core::menu::{self, MenuInputs};` (drop `MenuRow`); drop `MenuState` and `render_menu` from the `crate::popup` import; add

```rust
use geode_shell::keymap::Binding;
use geode_tile::confirm::{self, Confirm, ConfirmHost};
use geode_tile::menu::{Menu, MenuHost, MenuIds};
use geode_tile::notice::Notice;
```

New fields (next to `menu_tip_selector`), initialized in `new`:

```rust
    /// The action menu's element names, prepared once from the tile id.
    menu_ids: MenuIds,
    /// The keymap as last published, for the menu's key hints.
    chords: Arc<Vec<Binding>>,
```
```rust
            menu_ids: MenuIds::new(
                format!("marketdata-menu-{}", id.0),
                format!("marketdata-menu-row-{}", id.0),
            ),
            chords: geode_tile::menu::live_bindings(cx),
```

and an observer beside the `AppClock` one:

```rust
        // A keymap reload re-resolves an open menu's hints at once.
        cx.observe_global::<geode_shell::tips::Chords>(|this, cx| {
            this.chords = geode_tile::menu::live_bindings(cx);
            if let Some(Popup::Menu(m)) = &mut this.popup {
                m.rehint(&this.chords);
                cx.notify();
            }
        })
        .detach();
```

`toggle_menu`: replace the last two statements with `self.popup = Some(Popup::Menu(Menu::new(rows, &self.chords)));` followed by the existing `cx.notify();`. `dispatch` `"menu_down" | "menu_up"`: `Some(Popup::Menu(m)) => m.step(delta),`. `"menu_pick"`: `if let Some(index) = match &self.popup { Some(Popup::Menu(m)) => m.highlighted(), _ => None } { self.menu_pick(index, window, cx); }`. Delete the inherent `menu_hover` and `menu_pick` and add:

```rust
impl MenuHost for MarketDataTile {
    /// A disabled row's reason becomes the notice and the menu stays; an
    /// enabled row closes the menu and dispatches through the key's route.
    fn menu_pick(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(Popup::Menu(m)) = &self.popup else {
            return;
        };
        match m.pick(index) {
            Some(Err(reason)) => {
                self.notice = Some(reason);
                self.rebuild_chrome();
                cx.notify();
            }
            Some(Ok(id)) => {
                self.close_popup_with_window(window, cx);
                self.dispatch(&id, None, window, cx);
            }
            None => {}
        }
    }

    /// Change-only: gpui fires this on every pointer move over a row.
    fn menu_hover(&mut self, index: usize, cx: &mut Context<Self>) {
        if let Some(Popup::Menu(m)) = &mut self.popup
            && m.highlight(index)
        {
            cx.notify();
        }
    }
}
```

`render`: `Popup::Menu(m) => render_menu(m, &tile, self.id.0, cx).into_any_element(),` →

```rust
Popup::Menu(m) => geode_tile::menu::render_menu(
    m,
    &self.menu_ids,
    gpui::Anchor::TopRight,
    &tile,
    |t: &mut MarketDataTile, window, cx| t.close_popup_with_window(window, cx),
    cx,
)
.into_any_element(),
```

Test accessors: `menu_highlighted` → `Some(Popup::Menu(m)) => m.highlighted(),`; `menu_checks` → iterate `m.rows()` with `r.action().map(|a| (a.title().to_string(), a.checked()))`.

- [ ] **Step 7: Confirm in `tile.rs` and `header.rs`**

Replace the `PendingUpload` struct with the payload alone (keep the `draft` field's doc verbatim):

```rust
/// What an armed upload confirm asks to send: the rows assembled when it
/// was armed, so the prompt's counts and the payload describe one document.
/// The prompt, its focus and its blur answer are the confirm door's.
pub(crate) struct PendingUpload {
    target: String,
    rows: DocumentRows,
    /// (existing doc comment for `draft`, unchanged)
    draft: Draft,
}
```

Field: `pending_upload: Option<PendingUpload>` → `Option<Confirm<PendingUpload>>`. In `arm_upload`: delete `self.disarm_upload(window, cx);` (the door replaces an armed confirm) and replace from `let focus = cx.focus_handle();` through the `self.pending_upload = Some(PendingUpload { … });` statement with:

```rust
        confirm::arm(
            self,
            PendingUpload {
                target,
                rows,
                draft: self.draft.clone(),
            },
            prompt,
            window,
            cx,
        );
```

Delete `confirm_key`, `cancel_upload_on_pointer`, `disarm_upload`, `cancel_upload`. `submit_upload` takes the payload: `fn submit_upload(&mut self, pending: PendingUpload, cx: &mut Context<Self>) {` and drop its first three lines (`let Some(pending) = self.disarm_upload(window, cx) else { return; };`); the body is otherwise unchanged. Add:

```rust
impl ConfirmHost for MarketDataTile {
    type Payload = PendingUpload;

    fn confirm_slot(&mut self) -> &mut Option<Confirm<PendingUpload>> {
        &mut self.pending_upload
    }

    fn confirmed(&mut self, pending: PendingUpload, _: &mut Window, cx: &mut Context<Self>) {
        self.submit_upload(pending, cx);
    }

    fn cancelled(&mut self, _: PendingUpload, _: &mut Window, cx: &mut Context<Self>) {
        self.notice = Some(UPLOAD_CANCELLED.into());
        self.changed(cx);
    }
}
```

`withdraw_upload_if_moved` becomes:

```rust
    fn withdraw_upload_if_moved(&mut self, painted: Option<DocumentBase>, cx: &mut Context<Self>) {
        let now = self.painted_snapshot().and_then(|s| base_of(&s));
        let moved = |c: &Confirm<PendingUpload>| c.payload().draft != self.draft || now != painted;
        if !self.pending_upload.as_ref().is_some_and(moved) {
            return;
        }
        if confirm::withdraw(self, cx).is_none() {
            return;
        }
        // The policy's own disclosure (`replace`'s count, `rebase`'s
        // dropped edits) is kept behind the cancellation, never lost to it.
        self.notice = Some(match self.notice.take() {
            Some(n) => format!("{UPLOAD_CANCELLED_ARRIVED}; {n}").into(),
            None => UPLOAD_CANCELLED_ARRIVED.into(),
        });
        self.changed(cx);
    }
```

(keep its doc comment, replacing the sentence about recording the window handle with "The confirm door withdraws the prompt: it drops the blur answer first and blurs through the recorded window handle, deferred.")

`holds_focus`: `.is_some_and(|p| p.focus.is_focused(window))` → `.is_some_and(|c| c.holds_focus(window))`. The two `p.prompt` reads (`HeaderInputs.prompt` and the test accessor) → `c.prompt_text()`. `render`: `self.pending_upload.as_ref().map(|p| &p.focus)` → `self.pending_upload.as_ref()`; replace the root's `.when(self.pending_upload.is_some(), |el| el.capture_any_mouse_down(…))` and its `cancel_tile` binding by wrapping the root chain: `confirm::cancel_on_press(v_flex()…, self.pending_upload.is_some(), &tile)`. Tests: `.focus.clone()` → `.focus_handle().clone()`.

`header.rs`: `render`'s `confirm: Option<&FocusHandle>` → `confirm: Option<&Confirm<PendingUpload>>` (import `geode_tile::confirm::{self, Confirm}` and `crate::tile::PendingUpload`); the prompt child becomes:

```rust
    if let (Some(_), Some(pending)) = (&h.prompt, confirm) {
        row = row.child(confirm::prompt(
            pending,
            tile,
            move || format!("marketdata-upload-confirm-{tile_id}"),
            theme,
        ));
    }
```

Notices: `HeaderInputs.notice/upload_error` stay `Option<&SharedString>`; `HeaderModel.notice: Option<Notice>`, `HeaderModel.upload_error: Option<Notice>`; in `prepare`: `notice: i.notice.cloned().map(Notice::danger),` and `upload_error: i.upload_error.cloned().map(Notice::danger),`. `texts()`: `.text().to_string()` for both. In `render`:

```rust
    if let Some(e) = &h.upload_error {
        row = row.child(
            notice::render(e, theme)
                .debug_selector(move || format!("marketdata-upload-error-{tile_id}")),
        );
    }
    if let Some(n) = &h.notice {
        row = row.child(notice::render(n, theme));
    }
```

(import `geode_tile::notice::{self, Notice}`). Any test reading `header.notice`/`upload_error` as text uses `.as_ref().map(|n| n.text().to_string())`.

- [ ] **Step 8: Run the market-data suite**

Run: `cargo test -p geode-marketdata`
Expected: PASS, including the two new tests, every `panel:` confirm test, `hovering_a_menu_row_moves_the_highlight_and_occludes_the_grid`, `enter_on_a_greyed_row_notices_and_keeps_the_menu`, `the_menu_ticks_the_policy_and_a_pick_sets_it`, `a_rebase_under_the_question_withdraws_the_confirm`, `the_tile_answers_keys_after_the_upload_confirm_ends`.

- [ ] **Step 9: Docs**

`crates/geode-marketdata/README.md`: layout rows — `core::cursor, core::menu`: "…and the action list's rows (`geode_tile::menu` rows over action ids, hints as live chords)"; `header`: "…draft/upload feedback (notices through `geode_tile::notice`), …"; `popup`: "Underlying picker and cell-choice state and rendering over `geode_tile::popover`; the action menu is `geode_tile::menu`'s." Input and popup contracts: after "confirmation consumes every key, including chords, while armed." add "The upload confirm is `geode_tile::confirm`'s." and replace the action-menu paragraph with "Action-menu stepping, hover, picking and painting are `geode_tile::menu`'s: motion counts enabled actions and skips disabled rows, headings and separators without wrapping; hover can light a refused action so its reason stays readable; key hints are the live keymap's and follow a reload while the menu is open."

`crates/geode-pricer/README.md`: delete the Known-limitations bullet left by Task 4 ("The market-data action list's key hints …").

`docs/current/input-and-dialogs.md`: "…does not change the empty-state hint's `ctrl+k`; the palette, keybinding rows, tooltips, the timeseries footer, and every module's action menu read the live keymap."

- [ ] **Step 10: Re-aim the market-data harness entries**

```sh
run_mutation "mdmenu: a greyed row is a notice, not a dispatch" \
  crates/geode-tile/src/menu/mod.rs \
  '            Err(reason) => Err(reason.clone()),' \
  '            Err(_) => Ok(action.pick.clone()),' \
  geode-marketdata enter_on_a_greyed_row_notices_and_keeps_the_menu
```
```sh
run_mutation "mdpark: the menu's load row stays live on a dirty draft" \
  crates/geode-marketdata/src/core/menu.rs \
  '            Hint::chord("marketdata::load_underlying"),
            Ok(()),' \
  '            Hint::chord("marketdata::load_underlying"),
            if dirty { Err("revert or upload first") } else { Ok(()) },' \
  geode-marketdata a_dirty_draft_leaves_load_live_and_a_built_upload_is_live
```
```sh
run_mutation "mdmenu: the popup occludes what is painted beneath it" \
  crates/geode-tile/src/menu/render.rs \
  '        // Occlude what the menu covers so its hover and presses do not also reach it.
        .occlude()' \
  '        // Occlude what the menu covers so its hover and presses do not also reach it.' \
  geode-marketdata \
  hovering_a_menu_row_moves_the_highlight_and_occludes_the_grid
```
```sh
run_mutation "mdmenu: hovering a menu row moves the highlight" \
  crates/geode-tile/src/menu/mod.rs \
  '        self.highlighted = Some(index);
        true' \
  '        let _ = index;
        false' \
  geode-marketdata \
  hovering_a_menu_row_moves_the_highlight_and_occludes_the_grid
```
```sh
run_mutation "mdauto: exactly one policy row is checked" \
  crates/geode-marketdata/src/core/menu.rs \
  '                .checked(p == policy),' \
  '                .checked(true),' \
  geode-marketdata \
  exactly_one_policy_row_is_checked_and_it_follows_the_policy
```
```sh
run_mutation "mdmenu: stepping skips disabled rows" \
  crates/geode-tile/src/menu/mod.rs \
  '            (at + 1..rows.len()).find(|&i| rows[i].lands())' \
  '            (at + 1..rows.len()).find(|&i| rows[i].is_action())' \
  geode-marketdata \
  navigation_skips_disabled_rows
```
```sh
run_mutation "mdmenu: menu_down skips disabled rows in the tile" \
  crates/geode-tile/src/menu/mod.rs \
  '            (at + 1..rows.len()).find(|&i| rows[i].lands())' \
  '            (at + 1..rows.len()).find(|&i| rows[i].is_action())' \
  geode-marketdata \
  the_menu_ticks_the_policy_and_a_pick_sets_it
```
```sh
run_mutation "mdmenu: stepping skips separators and sections" \
  crates/geode-tile/src/menu/mod.rs \
  '            (at + 1..rows.len()).find(|&i| rows[i].lands())' \
  '            (at + 1..rows.len()).next()' \
  geode-marketdata \
  navigation_skips_separators_and_starts_on_the_first_enabled_row
```
```sh
run_mutation "panel: the confirm consumes a non-y key" \
  crates/geode-tile/src/confirm.rs \
  '            if tile.update(cx, |t, cx| key(t, event, window, cx)) {
                cx.stop_propagation();' \
  '            if tile.update(cx, |t, cx| key(t, event, window, cx)) {' \
  geode-marketdata \
  any_other_key_cancels_the_confirm_and_is_consumed
```
```sh
run_mutation "panel: a rebase under the confirm withdraws it" \
  crates/geode-marketdata/src/tile.rs \
  '        let moved = |c: &Confirm<PendingUpload>| c.payload().draft != self.draft || now != painted;' \
  '        let moved = |c: &Confirm<PendingUpload>| !same_edits(&c.payload().draft, &self.draft);' \
  geode-marketdata \
  a_rebase_under_the_question_withdraws_the_confirm
```
```sh
run_mutation "panel: the tile answers keys after the upload confirm ends" \
  crates/geode-tile/src/confirm.rs \
  '        if self.focus.is_focused(window) {' \
  '        if self.focus.is_focused(window) { window.disable_focus(cx); } if false {' \
  geode-marketdata \
  the_tile_answers_keys_after_the_upload_confirm_ends
```
```sh
run_mutation "panel: focus loss cancels the upload confirm" \
  crates/geode-tile/src/confirm.rs \
  '    let blur = cx.on_blur(&focus, window, |host: &mut T, window, cx| {
        cancel(host, window, cx);
    });' \
  '    let blur = cx.on_blur(&focus, window, |_: &mut T, _, _| {});' \
  geode-marketdata \
  focus_leaving_the_tile_cancels_the_confirm
```

Add the deliberate-change entry:

```sh
# The action list's hints are the live keymap's.
run_mutation "mdmenu: a rebind does not reach the menu" \
  crates/geode-marketdata/src/tile.rs \
  '            this.chords = geode_tile::menu::live_bindings(cx);' \
  '            let _ = geode_tile::menu::live_bindings(cx);' \
  geode-marketdata an_open_menu_follows_a_keymap_reload
```

- [ ] **Step 11: Gates**

```sh
cargo test -p geode-marketdata
cargo test -p geode-tile
cargo clippy -p geode-marketdata --all-targets -- -D warnings
cargo fmt --check
zsh scripts/mutation-check.sh --anchors-only
```

- [ ] **Step 12: Commit and verify**

```bash
git add crates/geode-marketdata crates/geode-pricer/README.md docs/current/input-and-dialogs.md scripts/mutation-check.sh Cargo.lock
git commit -m "refactor(marketdata): menu, upload confirm, popups and notices through geode-tile

Menu key hints now follow the live keymap (new: a_menu_hint_follows_a_user_rebind,
an_open_menu_follows_a_keymap_reload); core menu tests read door rows
(first_enabled and step answer Option).

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
pgrep -f 'mutation-che[c]k'
zsh scripts/mutation-check.sh --build-check "mdmenu:"
zsh scripts/mutation-check.sh --build-check "mdpark: the menu"
zsh scripts/mutation-check.sh --build-check "mdauto: exactly"
zsh scripts/mutation-check.sh --build-check "panel: "
zsh scripts/mutation-check.sh "mdmenu:"
zsh scripts/mutation-check.sh "mdpark: the menu"
zsh scripts/mutation-check.sh "mdauto: exactly"
zsh scripts/mutation-check.sh "panel: the confirm consumes"
zsh scripts/mutation-check.sh "panel: a rebase under"
zsh scripts/mutation-check.sh "panel: the tile answers keys"
zsh scripts/mutation-check.sh "panel: focus loss"
```
Expected: no BUILD; `caught` for each. Hand-apply each re-aimed replacement once; restore.

---

### Task 6: Timeseries migration (popover, menu, notice)

**Files:**
- Modify: `crates/geode-timeseries/Cargo.toml`, `src/core/menu.rs`, `src/popup.rs`, `src/header.rs`, `src/tile/mod.rs`, `src/tile/popups.rs`, `src/tile/tests.rs`, `README.md`; `docs/current/features.md`; `scripts/mutation-check.sh`

**Interfaces:**
- Consumes: Tasks 1, 2 and the notice door.
- Produces: `core::menu::{rows, range_rows, frequency_rows} -> Vec<Row<Pick>>`; `core::menu::start(&[Row<Pick>]) -> Option<usize>`; `core::menu::custom_row(&[Row<Pick>]) -> Option<usize>`; `impl MenuPick for Pick`; `popup::MenuState { kind: MenuKind, ids: MenuIds, menu: Menu<Pick> }`.

- [ ] **Step 1: Dependency**

`crates/geode-timeseries/Cargo.toml`: `geode-tile.workspace = true` after `geode-shell.workspace = true`; comment "…geode-tile the menus, popups and the notice line…".

- [ ] **Step 2: Write the failing test (keymap reload under an open menu)**

In `crates/geode-timeseries/src/tile/tests.rs` add (the harness already installs `Chords` for the footer tests; reuse its helper if one exists, otherwise add this one):

```rust
fn publish_chords(vcx: &mut gpui::VisualTestContext, user: Option<&str>) {
    let mut registry = geode_shell::actions::ActionRegistry::default();
    geode_shell::defaults::register_builtin_actions(&mut registry);
    for (id, title) in crate::content::ACTIONS {
        registry
            .register(geode_shell::actions::ActionDef {
                id: geode_shell::actions::ActionId(id.to_string()),
                title: title.to_string(),
                category: "Timeseries".into(),
            })
            .unwrap();
    }
    let mut docs = vec![
        geode_shell::keymap::fragments::fragment_doc("timeseries", crate::content::DEFAULT_KEYMAP)
            .unwrap(),
    ];
    if let Some(text) = user {
        docs.push(geode_core::config::LayerDoc {
            layer: geode_core::config::Layer::User,
            name: "keymap".into(),
            file: "user/keymap.toml".into(),
            table: text.parse().unwrap(),
        });
    }
    let (keymap, diags) =
        geode_shell::keymap::build_keymap(&docs, geode_shell::defaults::default_mod(), &registry);
    assert!(diags.is_empty(), "{diags:?}");
    vcx.update(|_, cx| {
        cx.set_global(geode_shell::tips::Chords(std::sync::Arc::new(
            keymap.bindings().to_vec(),
        )))
    });
    vcx.run_until_parked();
}

#[gpui::test]
fn an_open_menu_follows_a_keymap_reload(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open(cx);
    publish_chords(&mut vcx, None);
    h.keys(&mut vcx, "r");
    let lane = |h: &Harness, vcx: &gpui::VisualTestContext| {
        h.tile.read_with(vcx, |t, _| match t.popup() {
            Some(Popup::Menu(m)) => m
                .menu
                .rows()
                .iter()
                .filter_map(|r| r.action())
                .find(|a| a.title().as_ref() == "Custom dates…")
                .map(|a| a.lane().clone()),
            _ => None,
        })
    };
    let c = geode_shell::keymap::parse_binding("c", geode_shell::keymap::Modifiers::NONE).unwrap();
    assert_eq!(lane(&h, &vcx), Some(geode_tile::menu::Lane::Keys(c)));
    publish_chords(
        &mut vcx,
        Some("[[bindings]]\ncontext = \"timeseries && mode == normal && popup == menu && menu == range\"\n[bindings.keys]\n\"c\" = \"none\"\n\"shift+c\" = \"timeseries::range_custom\"\n"),
    );
    let shift_c =
        geode_shell::keymap::parse_binding("shift+c", geode_shell::keymap::Modifiers::NONE).unwrap();
    assert_eq!(lane(&h, &vcx), Some(geode_tile::menu::Lane::Keys(shift_c)));
}
```

(The user layer's context is the one `content.rs`'s `DEFAULT_KEYMAP` binds `c` under, verbatim; `register_builtin_actions` covers any shell action the fragment binds.)

- [ ] **Step 3: Run to verify it fails**

Run: `cargo test -p geode-timeseries an_open_menu_follows_a_keymap_reload`
Expected: FAIL to compile (`m.menu` unknown field).

- [ ] **Step 4: `core/menu.rs` on door rows**

Imports: drop `Keystroke, Modifiers`; add `use geode_tile::menu::{ActionRow, Hint, MenuPick, Row, first_enabled};`. Delete `MenuRow`, `Trailing`, `impl MenuRow`, `lands`, `step` and the local `first_enabled`. Add:

```rust
impl MenuPick for Pick {
    fn element_name(&self) -> SharedString {
        match self {
            Pick::Action(id) => SharedString::from(id.0.clone()),
            Pick::Range(p) => format!("range-{}", p.as_str()).into(),
            Pick::CustomRange => SharedString::new_static("range-custom"),
            Pick::Frequency(f) => format!("freq-{}", f.as_str()).into(),
        }
    }
}

fn action(id: &'static str, title: impl Into<SharedString>, enabled: Result<(), &'static str>) -> Row<Pick> {
    Row::Action(
        ActionRow::new(Pick::Action(ActionId(id.to_string())), title)
            .hint(Hint::chord(id))
            .enabled(enabled.map_err(SharedString::new_static)),
    )
}

fn toggle(id: &'static str, title: &'static str, on: bool) -> Row<Pick> {
    Row::Action(
        ActionRow::new(Pick::Action(ActionId(id.to_string())), title)
            .hint(Hint::chord(id))
            .checked(on),
    )
}
```

`rows` returns `Vec<Row<Pick>>` (body unchanged except `MenuRow::Separator` → `Row::Separator`, `MenuRow::Section(h)` → `Row::Section(h)`). `range_rows`:

```rust
pub fn range_rows(current: &Range) -> Vec<Row<Pick>> {
    let mut out: Vec<Row<Pick>> = Preset::ALL
        .into_iter()
        .map(|p| {
            Row::Action(
                ActionRow::new(Pick::Range(p), SharedString::new_static(p.title()))
                    .hint(Hint::label(p.as_str()))
                    .checked(*current == Range::Relative(p)),
            )
        })
        .collect();
    out.push(Row::Separator);
    out.push(Row::Action(
        ActionRow::new(Pick::CustomRange, SharedString::new_static("Custom dates…"))
            .hint(Hint::chord_or_keys(CUSTOM_RANGE_ACTION, "c"))
            .checked(matches!(current, Range::Absolute { .. })),
    ));
    out
}
```

`frequency_rows`:

```rust
pub fn frequency_rows(
    current: Frequency,
    refusal: impl Fn(Frequency) -> Result<(), String>,
) -> Vec<Row<Pick>> {
    Frequency::ALL
        .into_iter()
        .map(|f| {
            Row::Action(
                ActionRow::new(Pick::Frequency(f), SharedString::new_static(frequency_title(f)))
                    .hint(Hint::label(f.as_str()))
                    .enabled(refusal(f).map_err(SharedString::from))
                    .short_reason(OVER_CAP)
                    .checked(f == current),
            )
        })
        .collect()
}
```

`start` and `custom_row`:

```rust
/// Where a menu's highlight starts: on the ticked CHOICE when it can be
/// picked (the value in force — `Custom dates…` while the range is two
/// dates), else on the first enabled row. The action list ticks only
/// toggles, which are not choices, so it starts on its first enabled row.
pub fn start(rows: &[Row<Pick>]) -> Option<usize> {
    rows.iter()
        .position(|r| {
            r.action().is_some_and(|a| {
                a.checked() == Some(true)
                    && a.is_enabled()
                    && matches!(a.pick(), Pick::Range(_) | Pick::CustomRange | Pick::Frequency(_))
            })
        })
        .or_else(|| first_enabled(rows))
}

/// The `Custom dates…` row's index — where `escape` out of the date editor
/// puts the highlight back.
pub fn custom_row(rows: &[Row<Pick>]) -> Option<usize> {
    rows.iter()
        .position(|r| r.action().is_some_and(|a| matches!(a.pick(), Pick::CustomRange)))
}
```

Update the doc comment of `CUSTOM_RANGE_ACTION`: "The action id `Custom dates…` shares with its `c` key; the row's hint is that action's live chord, `c` when the keymap binds it nowhere." Tests in this file: add `use geode_tile::menu::{Menu, Trailing, step};`; rewrite helpers over door rows (`titles`, `row` by `Pick::Action(i) if i.0 == id`, `enabled` via `a.reason()`, `checked_of` via `a.checked()`, `pick_of` via `a.pick()`); `trail`/`hint_of` read resolved rows:

```rust
    fn resolved(rows: Vec<Row<Pick>>) -> Vec<Row<Pick>> {
        Menu::new(rows, &[]).rows().to_vec()
    }

    fn trail(row: &Row<Pick>) -> String {
        match row.action().map(|a| a.trailing()) {
            Some(Trailing::Text(t)) => t.to_string(),
            Some(Trailing::Keys(keys)) => keys.iter().map(|k| format!("[{}]", k.key)).collect(),
            Some(Trailing::None) | None => String::new(),
        }
    }
```

and wrap `range_rows(..)`/`frequency_rows(..)` in `resolved(..)` in the two tests that read hints. `step(&rows, a, d)` → `step(&rows, Some(a), d)` with `Some(..)` expectations; `first_enabled(&rows)` → `Some(0)`; `start(&rows)` → `Some(i)`; `step(&rows, 4, 1) == 0` stays `Some(0)` (the timeseries rule, now the door's).

- [ ] **Step 5: `popup.rs`**

Imports: drop `Anchor`, `AnchoredPositionMode`, `anchored`, `deferred`, `px` if unused, and `MenuRow, Trailing` from the `core::menu` import; add `use geode_tile::menu::{Menu, MenuIds};` and `use geode_tile::popover::{self, anchor_popup, empty_row, row_shell};`. Delete `ROW_HEIGHT`, `ROW_INSET`, `MIN_WIDTH` (import `ROW_HEIGHT`/`ROW_INSET` from `geode_tile::popover` where the dates editor or picker still use them), `popover_surface` (callers use `popover::surface`), `row_shell`, `empty_row`, `anchor_popup` (now imported) and `render_menu`. `MenuState`:

```rust
/// A menu's kind, its element names, and the door's menu (rows, including
/// hints and the frequency rows' cap refusals, and the one highlight
/// keyboard and pointer share). Rows refresh at open, on chrome rebuilds,
/// on frame changes and on a keymap publish.
pub(crate) struct MenuState {
    pub kind: MenuKind,
    pub ids: MenuIds,
    pub menu: Menu<Pick>,
}
```

(import `crate::core::menu::Pick`). Update the module doc's last lines: "Series, picker and completion rows share `geode_tile::popover::row_shell`; the menus are `geode_tile::menu`'s."

- [ ] **Step 6: Tile wiring (`tile/popups.rs`, `tile/mod.rs`)**

`open_menu`:

```rust
    pub(super) fn open_menu(&mut self, kind: MenuKind, cx: &mut Context<Self>) {
        let rows = self.menu_rows(kind, cx);
        let start = menu::start(&rows);
        let tile_id = self.id.0;
        self.popup = Some(Popup::Menu(MenuState {
            kind,
            ids: MenuIds::new(
                format!("ts-menu-{}-{tile_id}", kind.word()),
                format!("ts-menu-row-{tile_id}"),
            ),
            menu: Menu::new(rows, &geode_tile::menu::live_bindings(cx)).open_at(start),
        }));
        self.notice = None;
        cx.notify();
    }
```

`menu_rows` returns `Vec<Row<Pick>>` and loses its post-hoc hint loop (delete from `let empty = Vec::new();` to the end of the `for row in &mut rows` loop); its doc: "Hints are action identities the door resolves against the live keymap." `"list_down" | "list_up" if menu_open`: `m.menu.step(delta);`. `"menu_pick" if menu_open`: `let Some(index) = m.menu.highlighted() else { return true; };`. `back_to_range_menu`: `&& let Some(custom) = menu::custom_row(m.menu.rows())` and `{ m.menu.highlight(custom); }`. Delete the inherent `menu_hover` and `menu_pick` and add an `impl MenuHost for TimeseriesTile` whose `menu_hover` is `if let Some(Popup::Menu(m)) = &mut self.popup && m.menu.highlight(index) { cx.notify(); }` and whose `menu_pick` is the existing body with the row read replaced by:

```rust
        let Some(Popup::Menu(m)) = &self.popup else {
            return;
        };
        let pick = match m.menu.pick(index) {
            Some(Ok(pick)) => pick,
            Some(Err(reason)) => {
                self.notice = Some(reason);
                cx.notify();
                return;
            }
            None => return,
        };
        match pick {
            // the existing four arms, unchanged
        }
```

`refresh_menu_rows` (in `tile/mod.rs`):

```rust
    fn refresh_menu_rows(&mut self, cx: &App) -> bool {
        let Some(Popup::Menu(m)) = &self.popup else {
            return false;
        };
        let rows = self.menu_rows(m.kind, cx);
        let bindings = geode_tile::menu::live_bindings(cx);
        let Some(Popup::Menu(m)) = &mut self.popup else {
            return false;
        };
        m.menu.replace_rows(rows, &bindings)
    }
```

(doc: "…keeping the highlight on its row where that row is still an action, else snapping it to the nearest action. Answers whether the rows moved.") The `Chords` observer in `new` becomes:

```rust
        // The footer and an open menu name live chords, so a keymap reload
        // re-resolves both — once, here, never per frame.
        cx.observe_global::<geode_shell::tips::Chords>(|this, cx| {
            this.footer = header::footer_hints(cx);
            if let Some(Popup::Menu(m)) = &mut this.popup {
                m.menu.rehint(&geode_tile::menu::live_bindings(cx));
            }
            cx.notify();
        })
        .detach();
```

The painter call (wherever `crate::popup::render_menu(m, &tile, tile_id, cx)` is called from the header):

```rust
geode_tile::menu::render_menu(
    &m.menu,
    &m.ids,
    match m.kind {
        MenuKind::Actions => Anchor::TopRight,
        MenuKind::Range | MenuKind::Frequency => Anchor::TopLeft,
    },
    &tile,
    move |t: &mut TimeseriesTile, window, cx| t.outside_press(PopupKind::Menu(kind), window, cx),
    cx,
)
```

with `let kind = m.kind;` above it. `header.rs` `render_notice`:

```rust
pub(crate) fn render_notice(text: &SharedString, theme: &Theme) -> impl IntoElement {
    h_flex()
        .w_full()
        .px_2()
        .text_xs()
        .child(geode_tile::notice::paint(text, geode_tile::notice::Tone::Danger, theme))
}
```

(drop the now-unused `chip_paint` import if nothing else in the file uses it).

Tests (`tile/tests.rs`): `m.rows` → `m.menu.rows()`; `i == m.highlighted` → `Some(i) == m.menu.highlighted()`; `menu::MenuRow::Action { title, .. }` → `Row::Action(a)` + `a.title()`; `checked: Some(true)` match → `a.checked() == Some(true)`; `enabled: Ok(())` → `a.is_enabled()`; `m.rows[capped].trailing()` → `m.menu.rows()[capped].action().unwrap().trailing()` with `geode_tile::menu::Trailing::Text(text)`.

- [ ] **Step 7: Run the timeseries suite**

Run: `cargo test -p geode-timeseries`
Expected: PASS. If a test asserted the old refresh rule (a rebuilt menu's highlight falling to the first enabled row from a non-action index), update it to the snap rule and name it in the commit message.

- [ ] **Step 8: Docs**

`crates/geode-timeseries/README.md`: `core` row — "…the three menus' rows (action list, range, frequency) as `geode_tile::menu` rows over `Pick`…"; `popup` row — "State and rendering for the series list, add picker, custom dates editor and the expression field's completion list, plus expression-editor state; rows share `geode_tile::popover::row_shell`; the menus are `geode_tile::menu`'s." `docs/current/features.md` (≈ line 558): "Key hints refresh when the menu opens or its chrome rebuilds, so an open menu can retain old hints after a keymap reload." → "Key hints are the live keymap's and are re-resolved when the menu opens, when its chrome rebuilds and when the keymap is reloaded."

- [ ] **Step 9: Re-aim the timeseries harness entries**

```sh
run_mutation "timeseries range menu: the preset in force is ticked" \
  crates/geode-timeseries/src/core/menu.rs \
  '                    .checked(*current == Range::Relative(p)),' \
  '                    .checked(false),' \
  geode-timeseries \
  the_range_menu_writes_the_presets_out_ticks_the_current_and_ends_on_custom
```
```sh
run_mutation "timeseries range menu: an absolute range ticks custom dates" \
  crates/geode-timeseries/src/core/menu.rs \
  '            .checked(matches!(current, Range::Absolute { .. })),' \
  '            .checked(false),' \
  geode-timeseries \
  an_absolute_range_ticks_custom_dates_and_starts_there
```
```sh
run_mutation "timeseries menus: the highlight starts on the value in force" \
  crates/geode-timeseries/src/core/menu.rs \
  '                a.checked() == Some(true)
                    && a.is_enabled()' \
  '                a.checked() == Some(false)
                    && a.is_enabled()' \
  geode-timeseries \
  the_range_menu_writes_the_presets_out_ticks_the_current_and_ends_on_custom
```
```sh
run_mutation "timeseries mouse: menu stepping skips non-rows" \
  crates/geode-tile/src/menu/mod.rs \
  '            (at + 1..rows.len()).find(|&i| rows[i].lands())' \
  '            (at + 1..rows.len()).next()' \
  geode-timeseries stepping_skips_separators_and_sections_and_clamps
```
```sh
run_mutation "timeseries menu: stepping skips disabled rows" \
  crates/geode-tile/src/menu/mod.rs \
  '            (at + 1..rows.len()).find(|&i| rows[i].lands())' \
  '            (at + 1..rows.len()).find(|&i| rows[i].is_action())' \
  geode-timeseries stepping_skips_disabled_rows
```
```sh
run_mutation "timeseries menus: a short label is text, not a key" \
  crates/geode-tile/src/menu/mod.rs \
  '        Hint::Label(text) => Lane::Text(text.clone()),' \
  '        Hint::Label(_) => Lane::Empty,' \
  geode-timeseries the_range_menu_writes_the_presets_out_ticks_the_current_and_ends_on_custom
```
```sh
run_mutation "timeseries frequency menu: a capped row shows a short reason" \
  crates/geode-timeseries/src/core/menu.rs \
  '                    .short_reason(OVER_CAP)' \
  '' \
  geode-timeseries the_frequency_menu_ticks_the_current_and_disables_what_the_cap_refuses
```
```sh
run_mutation "timeseries frequency menu: a capped row is disabled" \
  crates/geode-timeseries/src/core/menu.rs \
  '                    .enabled(refusal(f).map_err(SharedString::from))' \
  '                    .enabled(Ok(()))' \
  geode-timeseries the_frequency_menu_ticks_the_current_and_disables_what_the_cap_refuses
```
```sh
run_mutation "timeseries menus: a disabled row explains and stays" \
  crates/geode-tile/src/menu/mod.rs \
  '            Err(reason) => Err(reason.clone()),' \
  '            Err(_) => Ok(action.pick.clone()),' \
  geode-timeseries the_actions_button_toggles_the_menu_and_a_row_click_dispatches_or_explains
```
```sh
run_mutation "timeseries dates editor: escape lands the highlight on custom dates" \
  crates/geode-timeseries/src/tile/popups.rs \
  '            m.menu.highlight(custom);' \
  '            let _ = custom;' \
  geode-timeseries \
  escape_in_the_editor_returns_to_the_range_menu_and_escape_again_closes
```

Add:

```sh
# An open menu's hints follow a keymap reload.
run_mutation "timeseries menus: a keymap reload leaves the open menu's hints" \
  crates/geode-timeseries/src/tile/mod.rs \
  '                m.menu.rehint(&geode_tile::menu::live_bindings(cx));' \
  '                let _ = &m.menu;' \
  geode-timeseries an_open_menu_follows_a_keymap_reload
```

- [ ] **Step 10: Gates, commit, verify**

```sh
cargo test -p geode-timeseries
cargo clippy -p geode-timeseries --all-targets -- -D warnings
cargo fmt --check
zsh scripts/mutation-check.sh --anchors-only
git add crates/geode-timeseries docs/current/features.md scripts/mutation-check.sh Cargo.lock
git commit -m "refactor(timeseries): menus, popups and the notice through geode-tile

An open menu now re-resolves its hints on a keymap reload (new:
an_open_menu_follows_a_keymap_reload).

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
pgrep -f 'mutation-che[c]k'
zsh scripts/mutation-check.sh --build-check "timeseries range menu:"
zsh scripts/mutation-check.sh --build-check "timeseries menus:"
zsh scripts/mutation-check.sh --build-check "timeseries frequency menu: a capped"
zsh scripts/mutation-check.sh --build-check "timeseries mouse: menu stepping"
zsh scripts/mutation-check.sh --build-check "timeseries menu: stepping"
zsh scripts/mutation-check.sh --build-check "timeseries dates editor: escape lands"
zsh scripts/mutation-check.sh "timeseries range menu: the preset"
zsh scripts/mutation-check.sh "timeseries range menu: an absolute"
zsh scripts/mutation-check.sh "timeseries menus:"
zsh scripts/mutation-check.sh "timeseries frequency menu: a capped"
zsh scripts/mutation-check.sh "timeseries mouse: menu stepping"
zsh scripts/mutation-check.sh "timeseries menu: stepping"
zsh scripts/mutation-check.sh "timeseries dates editor: escape lands"
```
Expected: no BUILD; `caught` each (the `timeseries menus:` substring also re-runs unchanged entries; all must stay `caught`). Hand-apply each re-aimed replacement once; restore.

---

### Task 7: Blotter notice, shared-rules docs, final gate

Diagnostics has no notice slot (Spec deviation 1); it is not touched.

**Files:**
- Modify: `crates/geode-blotter/Cargo.toml`, `src/tile.rs`, `README.md`; `crates/geode-tile/README.md`; `docs/current/features.md`; `scripts/mutation-check.sh`

**Interfaces:**
- Consumes: `geode_tile::notice::{self, Notice}`.
- Produces: `BlotterTile.error: Option<Notice>` (public field, same name).

- [ ] **Step 1: Dependency**

`crates/geode-blotter/Cargo.toml`: add `geode-tile.workspace = true` after `geode-shell.workspace = true`.

- [ ] **Step 2: Update the tests first (they pin the same tones through the new type)**

In `crates/geode-blotter/src/tile.rs` tests: every `Some(("binder error".to_string(), Tone::DangerText))` → `Some(Notice::danger("binder error"))`; the `Tone::WarningText` tuple near line 3637 → `Some(Notice::warning(<same text>))`; `t.error.as_ref().map(|e| e.0.to_string())` → `.map(|e| e.text().to_string())`; `.map(|e| e.0)` → `.map(|e| e.text().to_string())`. Add `use geode_tile::notice::Notice;` to the test module.

Run: `cargo test -p geode-blotter`
Expected: FAIL to compile (`error` is still `Option<(String, Tone)>`).

- [ ] **Step 3: Migrate the field**

`pub error: Option<(String, Tone)>` → `pub error: Option<Notice>` (doc: "Header notice. Dropped sorts and selections are warnings; query and configuration failures are danger."). Each assignment: `Some((x, Tone::WarningText))` → `Some(Notice::warning(x))`; `Some((x, Tone::DangerText))` → `Some(Notice::danger(x))` (lines ≈ 666, 681, 721, 761, 827, 1199). `error_text`: `self.error.as_ref().map(|e| e.text().to_string())`. Render (≈ line 1772):

```rust
        if let Some(n) = &self.error {
            header = header.child(notice::render(n, theme));
        }
```

Import `use geode_tile::notice::{self, Notice};`; keep the `chip::Tone` import only if other code still uses it (the `warn_text` line ≈ 1632 does).

- [ ] **Step 4: Run**

Run: `cargo test -p geode-blotter`
Expected: PASS.

- [ ] **Step 5: Docs**

`crates/geode-blotter/README.md`, `tile` row: append "The header notice is a `geode_tile::notice::Notice`." `crates/geode-tile/README.md`: add a "Users" line under the table: "Used by the pricer (all four doors), market-data (all four), timeseries (popover, menu, notice) and the blotter (notice). Diagnostics has no notice slot." `docs/current/features.md`: add, before the first module section, a short section:

```markdown
## Shared tile interaction

Modules build their popups, `.` action menus, in-tile y/n confirms and
notice lines from `geode-tile`, so each rule below holds in every tile that
has the surface:

- A popup is deferred above the tile, occludes what it covers, and snaps
  inside the window with an 8-pixel margin.
- A menu's key hints are the live keymap's and follow a reload while the menu
  is open; an action bound nowhere shows its `:` verb or nothing. Keyboard
  stepping lands only on enabled actions, clamped; hover can light a
  disabled row, which takes no fill, and picking it shows its reason and
  keeps the menu open.
- A confirm holds the keyboard: bare `y` confirms; any other key, a pointer
  press on the tile, or focus leaving cancels. Each answer blurs the prompt
  before it goes, and the keyboard returns to the tile.
- A notice is a status (muted), a warning or a danger line; which of a
  tile's notices shows is the tile's own precedence.
```

- [ ] **Step 6: Re-aim the blotter harness entries**

```sh
run_mutation "tile: a query error keeps the last snapshot" \
  crates/geode-blotter/src/tile.rs \
  '                self.error = Some(Notice::danger(e));' \
  '                self.error = Some(Notice::danger(e));
                self.table
                    .update(cx, |t, _| *t.delegate_mut() = BlotterDelegate::new());' \
  geode-blotter \
  a_stale_outcome_is_dropped_an_error_keeps_the_last_snapshot_and_timing_is_recorded
```
```sh
run_mutation "blotter: a stopped query refusal reads as something else" \
  crates/geode-blotter/src/tile.rs \
  '            self.error = Some(Notice::danger(format!("query refused: {refusal}")));' \
  '            self.error = Some(Notice::danger({ let _ = refusal; "query refused".to_string() }));' \
  geode-blotter a_refused_query_says_busy_or_stopped
```

(Write the two assignment sites in Step 3 exactly as these anchors spell them.)

- [ ] **Step 7: Final gate (whole workspace)**

```sh
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --check
cargo check -p geode-shell --features test-support --all-targets
zsh scripts/mutation-check.sh --anchors-only
```
Expected: all pass. (`cargo bench --workspace --no-run` is left to CI; locally it rebuilds every bench.)

- [ ] **Step 8: Commit and verify**

```bash
git add crates/geode-blotter crates/geode-tile/README.md docs/current/features.md scripts/mutation-check.sh Cargo.lock
git commit -m "refactor(blotter): header notice through geode-tile; shared tile rules documented

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
pgrep -f 'mutation-che[c]k'
zsh scripts/mutation-check.sh --build-check "tile: a query error keeps"
zsh scripts/mutation-check.sh --build-check "blotter: a stopped query"
zsh scripts/mutation-check.sh "tile: a query error keeps"
zsh scripts/mutation-check.sh "blotter: a stopped query"
```
Expected: no BUILD; `caught` for both. Hand-apply each; restore.

**Display checks owed (hand to Matthew after merge):** the market-data and pricer menus with a rebound key; a disabled row's reason in each menu; the three notice tones in the pricer, market-data, timeseries and blotter headers; the upload and `:rm` confirms (prompt, `y`, `n`, a click); the unified menu presentation (Spec deviation 7): timeseries lane size and hover, the pricer lit-row lane color, hover/pressed on market-data and pricer rows.

---

## Self-review

- **Spec coverage.** §3 crate, dependencies, direction, rule, build hygiene, four modules and README → Task 1 (+ README growth in 2, 3, 7; CLAUDE.md and architecture in 1). §4.1 → Task 1. §4.2 rows/hint/enabled/short reason/checked, stepping, renderer rulings, keys unchanged → Task 2; pricer `View` → `checked` Actions in Task 4; timeseries extensions decided in Deviation 6. §4.3 → Task 3 (focus restore per Deviation 3; prompt tone per Deviation 4). §4.4 → Task 1; precedence kept in modules (Tasks 4, 5). §5 order pricer → market-data → timeseries → blotter (Tasks 4–7), diagnostics per Deviation 1; deliberate changes pinned (hint: Tasks 4, 5; stepping from structure: door, Deviation 2). §6 pure and GPUI tests → Tasks 2, 3, 1; module tests updated in the commit naming them; harness re-aims per the map. §7 docs → Tasks 1, 4, 5, 6, 7.
- **Type consistency.** `Menu::highlighted() -> Option<usize>`, `Menu::pick -> Option<Result<P, SharedString>>`, `ActionRow` builders and readers, `MenuIds::new`, `render_menu(menu, ids, corner, tile, on_outside, cx)`, `ConfirmHost { type Payload; confirm_slot; confirmed; cancelled }`, `confirm::{arm, key, cancel, withdraw, prompt, cancel_on_press}`, `Notice::{status, warning, danger, text, tone}` are used with these exact names in Tasks 4–7.
- **Anchors.** Every re-aimed anchor is quoted from code this plan writes verbatim; `--anchors-only` after each task checks each matches once. Shared door anchors appear under several packages/filters (ALSO, not REDUNDANT).
