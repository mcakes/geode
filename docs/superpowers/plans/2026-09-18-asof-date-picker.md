# As-of Date Picker Implementation Plan (Phase 3, slice 2)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A trader can set a historical as-of on a day other than today by typing a date (`YYYY-MM-DD`, optionally with a time) or by clicking a day in a calendar pane inside the as-of dialog — the keyboard path is grammar, the mouse path is its §17 twin, and both end in the same field text.

**Architecture:** The grammar grows in `geode_core::query::parse_as_of` (`YYYY-MM-DD` resolves to that day's last instant, local; `YYYY-MM-DD HH:MM[:SS]` to that local instant), so every consumer — the dialog, `:asof <text>`, the scope bar's readback — gets it at once. The dialog gains a gpui-kit `Calendar` beside the presets, backed by one `CalendarState` entity owned by `ShellView` (built once, like `dialog_input`); a day click rewrites the DATE PART of the field's text through one door (`asof_view::on_calendar_selected`, a pure `compose_with_date` plus the one `set_value` + re-resolve), typing mirrors the parsed date back onto the calendar, `live` hides the pane, and the calendar never keeps focus — the field gets it back on every click. Nothing else changes: presets, `live`, the stripe, undo.

**Tech Stack:** Rust, chrono 0.4 (`Local`, `NaiveDate`, `NaiveTime`), gpui-pre 0.3.5, gpui-component 0.6.2 (`gpui_component::calendar::{Calendar, CalendarState, CalendarEvent, Date}`, re-exported from gpui-base).

**Spec:** `docs/superpowers/specs/2026-09-17-geode-gpui-kit-upgrade-and-adoption-design.md` §5.2. One amendment recorded in Task 4: the spec says a bare `YYYY-MM-DD` resolves to "23:59:59.999"; the implementation resolves to `23:59:59` local (whole seconds — the same precision the dialog's preview, the scope bar and the status segment show, and a sub-second boundary buys nothing since generations are stamped to the second).

## Global Constraints

- Charter (`docs/PHILOSOPHY.md`): every action keyboard-reachable — the calendar is the mouse form of typing a date, never the only way; nothing may stall the render thread; per-frame heap churn is a defect — the calendar entity is built once at `ShellView` construction, never per render, and the dialog's `build` allocates only what it already allocated.
- Phase 4a ruling: times are the trader's LOCAL clock throughout — every new form parses on the local date/zone and maps to UTC through the existing `resolve_local` (a DST gap or overlap is an `Err` naming the text).
- Interaction-model spec §16: the shared `Input` is written by one door; for this dialog the field's raw text IS the value (`asof_view`'s module doc), so the calendar's write goes through `asof_view::on_calendar_selected` — the only `set_value` this slice adds — and re-resolves through `on_query_changed` immediately, because `InputState::set_value` emits no `Change`.
- Interaction-model spec §17 (mouse parity): a day click does exactly what typing that date would; the calendar takes no focus of its own — after the click the field is focused again.
- No raw colours: theme tokens only; the calendar paints itself from the theme.
- TDD: pure logic (`parse_as_of`, `compose_with_date`, `calendar_date`) is tested without a window; the dialog with `#[gpui::test]` in `crates/geode-shell/src/shell/tests/asof.rs` using the existing fixtures (`open_shell(cx, test_services())`, `shell_of`, `vcx.simulate_keystrokes("alt-t")`, `vcx.simulate_input("…")`). A day "click" in a test drives the calendar entity's `activate_date(date, cx)` — the same call the component's own day-cell `on_click` makes — because the day cells carry gpui-base's own ids, not Geode `debug_selector`s.
- The four CI checks plus the `test-support` check stay green; `zsh scripts/mutation-check.sh --anchors-only` exits 0; every behaviour this plan adds gets a harness entry (Task 4), and CLAUDE.md line 85's count is bumped to `grep -c "^run_mutation " scripts/mutation-check.sh`.
- Commits are small and per task; each ends with the harness-supplied `Co-Authored-By` trailer. Never commit `TODO.md` or `docs/modules.md`.
- Work on a worktree branch off `main` (`superpowers:using-git-worktrees` at execution time), named `worktree-asof-calendar`.
- Out of scope: the presets list, `live`, undo, the warning stripe, the status/scope-bar readouts (all unchanged); a time picker; keyboard navigation INSIDE the calendar (typing is the keyboard path).

---

## File Structure

| File | Responsibility |
|---|---|
| `crates/geode-core/src/query.rs` (`parse_as_of`, `resolve_local`, tests) | the grammar: three new forms, one error message (Task 1) |
| `crates/geode-shell/src/shell/asof_view.rs` | pure: `compose_with_date`, `AsOfState::calendar_date`, `shows_calendar`; gpui: the calendar pane in `build`, `on_calendar_selected` (Tasks 2–3) |
| `crates/geode-shell/src/shell/mod.rs` | `ShellView.as_of_calendar: Entity<CalendarState>` built in `new`, its `CalendarEvent` subscription, the typing → calendar mirror in the `dialog_input` subscription's as-of arm, a `test-support` accessor (Task 3) |
| `crates/geode-shell/src/shell/tests/asof.rs` | window tests (Task 3) |
| `scripts/mutation-check.sh`, `CLAUDE.md`, the spec | harness entries, count, §5.2 as-built (Task 4) |

---

### Task 1: The grammar — `YYYY-MM-DD`, `YYYY-MM-DD HH:MM`, `YYYY-MM-DD HH:MM:SS`

**Files:**
- Modify: `crates/geode-core/src/query.rs:198-230` (`parse_as_of`'s doc and body) and its `mod tests`
- Test: `crates/geode-core/src/query.rs` `mod tests`

**Interfaces:**
- Consumes: the existing `resolve_local(date: NaiveDate, time: NaiveTime, text: &str) -> Result<DateTime<Utc>, String>`.
- Produces: `parse_as_of(text, now)` accepting, beside today's forms, `%Y-%m-%d` (→ that local day at `23:59:59`), `%Y-%m-%d %H:%M` and `%Y-%m-%d %H:%M:%S` (→ that local instant). `pub const END_OF_DAY: NaiveTime` (23:59:59) so Task 2 can name the same default. The error message becomes `'{text}' is not HH:MM, YYYY-MM-DD[ HH:MM[:SS]] or an RFC 3339 time`.

- [ ] **Step 1: Write the failing tests**

In `query.rs`'s `mod tests`, beside the existing `parse_as_of` tests (they use an `expect_local(now, time)` helper that computes the expected local instant independently — read it; the new helper below is its date-taking sibling):

```rust
    /// The instant `parse_as_of` should produce for `date` + `time` in
    /// the machine's LOCAL zone, computed independently of the parser.
    fn expect_local_on(date: chrono::NaiveDate, time: NaiveTime) -> DateTime<Utc> {
        Local
            .from_local_datetime(&date.and_time(time))
            .single()
            .expect("test picks a time that exists in every zone")
            .to_utc()
    }

    #[test]
    fn a_bare_date_resolves_to_the_end_of_that_local_day() {
        let now = DateTime::parse_from_rfc3339("2026-09-18T10:00:00Z").unwrap().with_timezone(&Utc);
        let date = chrono::NaiveDate::from_ymd_opt(2026, 9, 8).unwrap();
        assert_eq!(
            parse_as_of("2026-09-08", now).unwrap(),
            expect_local_on(date, END_OF_DAY)
        );
        assert_eq!(END_OF_DAY, NaiveTime::from_hms_opt(23, 59, 59).unwrap());
    }

    #[test]
    fn a_date_with_a_time_resolves_to_that_local_instant() {
        let now = DateTime::parse_from_rfc3339("2026-09-18T10:00:00Z").unwrap().with_timezone(&Utc);
        let date = chrono::NaiveDate::from_ymd_opt(2026, 9, 8).unwrap();
        assert_eq!(
            parse_as_of("2026-09-08 14:05", now).unwrap(),
            expect_local_on(date, NaiveTime::from_hms_opt(14, 5, 0).unwrap())
        );
        assert_eq!(
            parse_as_of("2026-09-08 14:05:30", now).unwrap(),
            expect_local_on(date, NaiveTime::from_hms_opt(14, 5, 30).unwrap())
        );
    }

    #[test]
    fn a_date_form_ignores_today_entirely() {
        // `now` is on a different day; the parsed instant must not depend on it.
        let a = DateTime::parse_from_rfc3339("2026-01-01T00:00:00Z").unwrap().with_timezone(&Utc);
        let b = DateTime::parse_from_rfc3339("2026-12-31T23:00:00Z").unwrap().with_timezone(&Utc);
        assert_eq!(parse_as_of("2026-09-08 09:30", a), parse_as_of("2026-09-08 09:30", b));
    }

    #[test]
    fn an_impossible_date_or_a_slashed_one_is_an_error_naming_the_forms() {
        let now = Utc::now();
        let err = parse_as_of("2026-02-30", now).unwrap_err();
        assert!(err.contains("YYYY-MM-DD"), "{err}");
        let err = parse_as_of("2026/09/08", now).unwrap_err();
        assert!(err.contains("YYYY-MM-DD"), "{err}");
        assert!(err.contains("HH:MM"), "{err}");
    }

    #[test]
    fn the_existing_forms_still_parse() {
        let now = DateTime::parse_from_rfc3339("2026-09-18T10:00:00Z").unwrap().with_timezone(&Utc);
        assert!(parse_as_of("14:05", now).is_ok());
        assert!(parse_as_of("14:05:30", now).is_ok());
        assert!(parse_as_of("2026-09-08T14:05:00Z", now).is_ok());
        assert!(parse_as_of("2026-09-08T14:05:00+01:00", now).is_ok());
    }
```

Use `chrono::NaiveDate` via the path the file already imports (`grep -n "^use chrono" crates/geode-core/src/query.rs`).

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p geode-core query::tests 2>&1 | grep -E "error\[|cannot find|FAILED|^test result" | head`
Expected: compile error on `END_OF_DAY` (not yet defined).

- [ ] **Step 3: Implement**

Replace `parse_as_of` and its doc comment:

```rust
/// The last whole second of a day — what a bare `YYYY-MM-DD` as-of
/// means: "the newest generation of that day". Whole seconds, not
/// `.999`, because every displayed time in the app is whole-second and
/// generations are stamped to the second.
pub const END_OF_DAY: NaiveTime = match NaiveTime::from_hms_opt(23, 59, 59) {
    Some(t) => t,
    None => unreachable!(),
};

/// `HH:MM` or `HH:MM:SS` means today at that time on the trader's LOCAL
/// clock (the modal's presets, its preview, the scope bar and the status
/// segment all display local time — one clock throughout, spec §3.6);
/// `YYYY-MM-DD` means the end of that local day ([`END_OF_DAY`]);
/// `YYYY-MM-DD HH:MM` or `YYYY-MM-DD HH:MM:SS` means that local instant;
/// anything else must be RFC 3339. `now` stays UTC so callers and tests
/// keep their shape; it is converted to the local date internally and
/// is not consulted at all by the date-carrying forms. A local time that
/// does not exist or is ambiguous (a DST gap or overlap) is an `Err`
/// naming the time.
pub fn parse_as_of(text: &str, now: DateTime<Utc>) -> Result<DateTime<Utc>, String> {
    let today_local = now.with_timezone(&Local).date_naive();
    if let Ok(t) = NaiveTime::parse_from_str(text, "%H:%M") {
        return resolve_local(today_local, t, text);
    }
    if let Ok(t) = NaiveTime::parse_from_str(text, "%H:%M:%S") {
        return resolve_local(today_local, t, text);
    }
    if let Ok(d) = chrono::NaiveDate::parse_from_str(text, "%Y-%m-%d") {
        return resolve_local(d, END_OF_DAY, text);
    }
    if let Ok(dt) = chrono::NaiveDateTime::parse_from_str(text, "%Y-%m-%d %H:%M") {
        return resolve_local(dt.date(), dt.time(), text);
    }
    if let Ok(dt) = chrono::NaiveDateTime::parse_from_str(text, "%Y-%m-%d %H:%M:%S") {
        return resolve_local(dt.date(), dt.time(), text);
    }
    DateTime::parse_from_rfc3339(text)
        .map(|t| t.with_timezone(&Utc))
        .map_err(|_| format!("'{text}' is not HH:MM, YYYY-MM-DD[ HH:MM[:SS]] or an RFC 3339 time"))
}
```

If `NaiveTime::from_hms_opt` is not `const` at the pinned chrono (check `cargo doc` or try the build), fall back to `pub fn end_of_day() -> NaiveTime { NaiveTime::from_hms_opt(23, 59, 59).unwrap() }` and adjust the tests and Task 2 to call it. Order matters: `%H:%M` is tried before the date forms so `14:05` is never mis-read; `%Y-%m-%d` is tried before `%Y-%m-%d %H:%M` — `NaiveDate::parse_from_str` rejects trailing input, so a date-with-time never matches the bare-date arm (the test `a_date_with_a_time_resolves…` pins it).

- [ ] **Step 4: Run the tests**

Run: `cargo test -p geode-core query::tests 2>&1 | grep -E "^test |^test result"`
Expected: all `ok`, including the pre-existing `parse_as_of` tests (the error-message assertion in any existing test must still hold — if one asserted the OLD message text verbatim, update only that literal to the new message; that is not a weakening).

- [ ] **Step 5: Any other consumer of the old error text**

Run: `grep -rn "is not HH:MM" crates docs --include=*.rs --include=*.md | grep -v "query.rs"`. Update any test or doc literal naming the old message to the new one.

- [ ] **Step 6: fmt, clippy, commit**

Run: `cargo fmt --check && cargo clippy -p geode-core --all-targets -- -D warnings 2>&1 | tail -1`

```bash
git add crates/geode-core/src/query.rs
git commit -m "core: as-of grammar takes YYYY-MM-DD (end of that local day) and YYYY-MM-DD HH:MM[:SS]"
```

---

### Task 2: The dialog's pure half — `compose_with_date`, `calendar_date`, `shows_calendar`

**Files:**
- Modify: `crates/geode-shell/src/shell/asof_view.rs` (pure section, above the `gpui shell` divider) and its `mod tests`

**Interfaces:**
- Consumes: `geode_core::query::{parse_as_of, END_OF_DAY}`; `AsOfState { selected, error, resolved, presets_cache }`.
- Produces:
  - `pub fn compose_with_date(text: &str, date: NaiveDate) -> String` — the field text a day click yields: keeps a typed time (`14:05` or `14:05:30`, alone or after a date), otherwise the bare date.
  - `pub fn calendar_date(state: &AsOfState, now: DateTime<Utc>) -> NaiveDate` — the day the calendar highlights: `resolved`'s local date, else today's local date.
  - `pub fn shows_calendar(text: &str) -> bool` — `false` iff the trimmed text is `live` (any case).

- [ ] **Step 1: Failing unit tests**

In `asof_view.rs`'s `mod tests` (grep `#[cfg(test)]` there; the existing tests exercise `resolve_input`/`on_query_changed` — put these beside them):

```rust
    fn d(y: i32, m: u32, day: u32) -> chrono::NaiveDate {
        chrono::NaiveDate::from_ymd_opt(y, m, day).unwrap()
    }

    #[test]
    fn compose_keeps_a_typed_time_and_replaces_or_adds_the_date() {
        let day = d(2026, 9, 8);
        assert_eq!(compose_with_date("", day), "2026-09-08");
        assert_eq!(compose_with_date("   ", day), "2026-09-08");
        assert_eq!(compose_with_date("14:05", day), "2026-09-08 14:05");
        assert_eq!(compose_with_date("14:05:30", day), "2026-09-08 14:05:30");
        assert_eq!(compose_with_date("2026-01-01 09:30", day), "2026-09-08 09:30");
        assert_eq!(compose_with_date("2026-01-01", day), "2026-09-08");
    }

    #[test]
    fn compose_drops_text_that_is_neither_a_time_nor_a_date() {
        let day = d(2026, 9, 8);
        // Garbage, `live`, or an RFC 3339 instant: the click means "this
        // day", so the field becomes the bare date.
        assert_eq!(compose_with_date("nonsense", day), "2026-09-08");
        assert_eq!(compose_with_date("live", day), "2026-09-08");
        assert_eq!(compose_with_date("2026-01-01T09:30:00Z", day), "2026-09-08");
    }

    #[test]
    fn calendar_date_follows_the_resolved_instant_else_today() {
        let now = chrono::Utc::now();
        let mut state = AsOfState::default();
        assert_eq!(calendar_date(&state, now), now.with_timezone(&chrono::Local).date_naive());
        on_query_changed(&mut state, "2026-09-08 14:05", now);
        assert_eq!(calendar_date(&state, now), d(2026, 9, 8));
        on_query_changed(&mut state, "live", now);
        assert_eq!(calendar_date(&state, now), now.with_timezone(&chrono::Local).date_naive());
    }

    #[test]
    fn the_calendar_hides_only_under_live() {
        assert!(shows_calendar(""));
        assert!(shows_calendar("14:05"));
        assert!(shows_calendar("nonsense"));
        assert!(!shows_calendar("live"));
        assert!(!shows_calendar(" LIVE "));
    }
```

If `AsOfState` has no `Default`, construct it the way the existing tests do (grep `AsOfState {` in the tests).

- [ ] **Step 2: Run, expect compile failures; implement**

Above the `gpui shell` divider:

```rust
/// The field text a calendar day click yields (spec §5.2, §17 mouse
/// parity): a typed time — `HH:MM` or `HH:MM:SS`, alone or after a date
/// — is kept and the date part becomes `date`; anything else (blank,
/// `live`, an RFC 3339 instant, garbage) becomes the bare date, which
/// [`parse_as_of`] reads as the end of that day.
pub fn compose_with_date(text: &str, date: NaiveDate) -> String {
    let trimmed = text.trim();
    let time_part = trimmed
        .rsplit_once(' ')
        .map(|(_, t)| t)
        .unwrap_or(trimmed);
    let keeps_time = NaiveTime::parse_from_str(time_part, "%H:%M").is_ok()
        || NaiveTime::parse_from_str(time_part, "%H:%M:%S").is_ok();
    if keeps_time {
        format!("{} {time_part}", date.format("%Y-%m-%d"))
    } else {
        date.format("%Y-%m-%d").to_string()
    }
}

/// The day the calendar highlights: the field's resolved instant on the
/// trader's local clock, else today (local).
pub fn calendar_date(state: &AsOfState, now: DateTime<Utc>) -> NaiveDate {
    state
        .resolved
        .unwrap_or(now)
        .with_timezone(&Local)
        .date_naive()
}

/// The calendar is hidden while the field reads `live` — there is no
/// day to pick for "now".
pub fn shows_calendar(text: &str) -> bool {
    !text.trim().eq_ignore_ascii_case("live")
}
```

Add `use chrono::{NaiveDate, NaiveTime};` (and `Local` if not already imported). A `rsplit_once(' ')` on `"14:05"` yields `None` → the whole text is the candidate time; on `"2026-01-01 09:30"` yields `"09:30"`; on `"2026-01-01T09:30:00Z"` yields `None` and the RFC form fails both time parses → bare date. Nothing here allocates except the returned `String`, and it runs once per click.

- [ ] **Step 3: Run the tests; fmt; clippy; commit**

Run: `cargo test -p geode-shell asof_view::tests 2>&1 | grep -E "^test |^test result"` → all `ok`. `cargo fmt --check && cargo clippy -p geode-shell --all-targets -- -D warnings 2>&1 | tail -1`.

```bash
git add crates/geode-shell/src/shell/asof_view.rs
git commit -m "shell: as-of dialog pure half — compose_with_date, calendar_date, shows_calendar"
```

---

### Task 3: The calendar pane — entity, subscription, mirror, layout, focus

**Files:**
- Modify: `crates/geode-shell/src/shell/mod.rs` (`ShellView` fields ~line 619/888; `new` ~1077–1120; the `dialog_input` subscription's as-of arm ~1112; the test-support accessors ~1774)
- Modify: `crates/geode-shell/src/shell/asof_view.rs` (`open`, `build`, new `on_calendar_selected`)
- Test: `crates/geode-shell/src/shell/tests/asof.rs`

**Interfaces:**
- Consumes: Task 2's three functions; `gpui_component::calendar::{Calendar, CalendarState, CalendarEvent, Date}`; `dialog::filter_row`; the existing `open` door and `dialog_input` subscription.
- Produces:
  - `ShellView.as_of_calendar: Entity<CalendarState>` (private; `#[cfg(any(test, feature = "test-support"))] pub fn as_of_calendar(&self) -> &Entity<CalendarState>`).
  - `pub(crate) fn asof_view::on_calendar_selected(view: &mut ShellView, date: NaiveDate, window: &mut Window, cx: &mut Context<ShellView>)` — the one write door.
  - Selector `as-of-calendar` on the pane's wrapper (present iff `shows_calendar`).

- [ ] **Step 1: Failing window tests**

In `crates/geode-shell/src/shell/tests/asof.rs`, beside the existing tests (fixtures: `open_shell(cx, test_services())`, `shell_of(&window, &mut vcx)`, `vcx.simulate_keystrokes("alt-t")`, `vcx.simulate_input("…")`):

```rust
#[gpui::test]
fn the_as_of_dialog_paints_a_calendar_that_live_hides(cx: &mut gpui::TestAppContext) {
    let (window, mut vcx) = open_shell(cx, test_services());
    let _shell = shell_of(&window, &mut vcx);
    vcx.simulate_keystrokes("alt-t");
    assert!(vcx.debug_bounds("as-of-calendar").is_some(), "calendar pane painted on open");
    vcx.simulate_input("live");
    vcx.run_until_parked();
    assert!(vcx.debug_bounds("as-of-calendar").is_none(), "hidden under live");
    vcx.simulate_keystrokes("backspace backspace backspace backspace");
    vcx.run_until_parked();
    assert!(vcx.debug_bounds("as-of-calendar").is_some(), "back once live is gone");
}

#[gpui::test]
fn clicking_a_day_writes_the_date_and_keeps_a_typed_time(cx: &mut gpui::TestAppContext) {
    let (window, mut vcx) = open_shell(cx, test_services());
    let shell = shell_of(&window, &mut vcx);
    vcx.simulate_keystrokes("alt-t");
    let day = chrono::NaiveDate::from_ymd_opt(2026, 9, 8).unwrap();
    // A "click" is the same call the calendar's own day cell makes.
    let calendar = shell.read_with(&vcx, |s, _| s.as_of_calendar().clone());
    calendar.update(&mut vcx, |c, cx| { c.activate_date(day, cx); });
    vcx.run_until_parked();
    let text = shell.read_with(&vcx, |s, cx| s.dialog_input().read(cx).value().to_string());
    assert_eq!(text, "2026-09-08", "a blank field becomes the bare date");
    assert!(vcx.debug_bounds("as-of-resolved").is_some(), "the preview shows the end of that day");
    // Now type a time, click another day: the time survives.
    vcx.simulate_keystrokes("ctrl-a backspace");
    vcx.simulate_input("14:05");
    let other = chrono::NaiveDate::from_ymd_opt(2026, 9, 9).unwrap();
    calendar.update(&mut vcx, |c, cx| { c.activate_date(other, cx); });
    vcx.run_until_parked();
    let text = shell.read_with(&vcx, |s, cx| s.dialog_input().read(cx).value().to_string());
    assert_eq!(text, "2026-09-09 14:05");
    // The field still has the keyboard: enter commits.
    vcx.simulate_keystrokes("enter");
    let frame = shell.read_with(&vcx, |s, _| s.frame().clone());
    assert!(matches!(frame.read_with(&vcx, |f, _| f.as_of().clone()), AsOf::At(_)));
    assert!(shell.read_with(&vcx, |s, _| s.modal.is_none()));
}

#[gpui::test]
fn typing_a_date_moves_the_calendars_selection(cx: &mut gpui::TestAppContext) {
    let (window, mut vcx) = open_shell(cx, test_services());
    let shell = shell_of(&window, &mut vcx);
    vcx.simulate_keystrokes("alt-t");
    vcx.simulate_input("2026-09-08 14:05");
    vcx.run_until_parked();
    let calendar = shell.read_with(&vcx, |s, _| s.as_of_calendar().clone());
    let selected = calendar.read_with(&vcx, |c, _| c.date().start());
    assert_eq!(selected, chrono::NaiveDate::from_ymd_opt(2026, 9, 8));
}
```

`shell.dialog_input()` — if no such test-support accessor exists, add one beside `picker()` (`pub fn dialog_input(&self) -> &Entity<InputState>`); if the tests elsewhere read the field another way (grep `dialog_input` in `shell/tests/`), use that. `ctrl-a backspace` clears the field only if the Input binds `ctrl-a` to select-all in this context — the dialog reclaims `ctrl+a`; if the clear does not work, use `backspace` × the text length as the first test does.

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p geode-shell the_as_of_dialog_paints_a_calendar clicking_a_day typing_a_date_moves 2>&1 | grep -E "error\[|panicked|^test result" | head -5`
Expected: compile errors on `as_of_calendar`.

- [ ] **Step 3: The entity on `ShellView`**

In `shell/mod.rs`: add the field next to `as_of_dialog`:

```rust
    /// The as-of dialog's calendar (spec §5.2), built once here like
    /// `dialog_input` — a `CalendarState` is an entity with its own focus
    /// handle, and creating one per open would leak a focus handle per
    /// dialog. It is painted only while the dialog is open and the field
    /// does not read `live`; its selection mirrors the field.
    as_of_calendar: Entity<gpui_component::calendar::CalendarState>,
```

In `new`, after `dialog_input` is created and subscribed:

```rust
        let as_of_calendar = cx.new(|cx| gpui_component::calendar::CalendarState::new(window, cx));
        cx.subscribe_in(&as_of_calendar, window, |view, _, event, window, cx| {
            let gpui_component::calendar::CalendarEvent::Selected(date) = event;
            if let Some(day) = date.start() {
                asof_view::on_calendar_selected(view, day, window, cx);
            }
        })
        .detach();
```

and store it in the struct literal. In the `dialog_input` subscription's as-of arm (the `else if let Some(state) = view.as_of_dialog.as_mut()` block), after `asof_view::on_query_changed(state, &query, chrono::Utc::now());` add the mirror:

```rust
                // Typing mirrors onto the calendar (spec §5.2): the parsed
                // day, else today. `set_date` notifies the calendar only.
                let day = asof_view::calendar_date(state, chrono::Utc::now());
                view.as_of_calendar.update(cx, |c, cx| {
                    if c.date().start() != Some(day) {
                        c.set_date(day, _window, cx);
                    }
                });
```

(`_window` is the subscription closure's window parameter — rename it `window` if it is currently `_window`.) The guard avoids a notify per keystroke when the day is unchanged. Add the test-support accessor beside `picker()`:

```rust
    #[cfg(any(test, feature = "test-support"))]
    pub fn as_of_calendar(&self) -> &Entity<gpui_component::calendar::CalendarState> {
        &self.as_of_calendar
    }
```

- [ ] **Step 4: The write door and the pane**

In `asof_view.rs`, in the gpui section:

```rust
/// A calendar day was clicked (the `CalendarEvent::Selected` subscription,
/// `shell/mod.rs`): rewrite the field's DATE PART through the one door
/// this dialog has for writing the shared `Input` — [`compose_with_date`]
/// keeps a typed time — then re-resolve at once, because
/// `InputState::set_value` emits no `Change` (the trap `sync_dialog_text`
/// documents), and hand focus back to the field: the calendar is the
/// mouse form of typing a date, never a focus owner (spec §5.2, §17).
pub(crate) fn on_calendar_selected(
    view: &mut ShellView,
    date: NaiveDate,
    window: &mut Window,
    cx: &mut Context<ShellView>,
) {
    if view.as_of_dialog.is_none() {
        return;
    }
    let input = view.dialog_input.clone();
    let current = input.read(cx).value().to_string();
    let next = compose_with_date(&current, date);
    input.update(cx, |i, cx| i.set_value(next.clone(), window, cx));
    if let Some(state) = view.as_of_dialog.as_mut() {
        on_query_changed(state, &next, chrono::Utc::now());
    }
    input.read(cx).focus_handle(cx).focus(window, cx);
    cx.notify();
}
```

(`view.dialog_input` and `view.as_of_dialog` are private fields of `ShellView` in `shell/mod.rs`; `asof_view` is a child module of `shell`, so it can read them — every other dialog module here does.) In `build`, replace the tail so the presets and the calendar sit side by side, the calendar hidden under `live`:

```rust
    let presets_list = cached_presets(state, shell.frame.read(cx));
    let list = build_presets(&presets_list, state.selected, entity, primary, muted, selection);
    let text = shell.dialog_input.read(cx).value().to_string();
    let body = if shows_calendar(&text) {
        h_flex()
            .gap_3()
            .items_start()
            .child(div().flex_1().min_w_0().child(list))
            .child(
                div()
                    .flex_none()
                    .debug_selector(|| "as-of-calendar".to_string())
                    .child(Calendar::new(&shell.as_of_calendar).small()),
            )
            .into_any_element()
    } else {
        list.into_any_element()
    };
    column.child(body).into_any_element()
```

The `value().to_string()` there is one small allocation per dialog paint — the dialog already clones strings per paint (`err.clone()`, the preview `format!`), and a paint happens on keystrokes, not per frame; note it in a comment and prefer `shell.dialog_input.read(cx).value()` borrowed if `shows_calendar` can take `&str` from it without the copy (it can: `shows_calendar(shell.dialog_input.read(cx).value())` — do that instead if `value()` returns `&str`/`&SharedString`). Import `gpui_component::calendar::Calendar` and `gpui_component::Sizable as _`. If `WIDTH` (480) is too narrow for the presets plus a small calendar, widen the dialog to 640 and say so in the `WIDTH` doc comment. In `open`, after the state is constructed, seed the calendar to today: `view.as_of_calendar.update(cx, |c, cx| c.set_date(chrono::Local::now().date_naive(), window, cx));` (the field opens blank, so the calendar shows today).

- [ ] **Step 5: Run the three tests, then the crate**

Run: `cargo test -p geode-shell the_as_of_dialog_paints_a_calendar clicking_a_day typing_a_date_moves 2>&1 | grep -E "^test |panicked"` → all `ok`. If `activate_date` in the test does not emit (read `gpui-base-0.6.2/src/calendar.rs:258-266`: it emits `Selected` when the date is selectable and the view is the day view), call `select_date` + `cx.emit(CalendarEvent::Selected(c.date()))` in the test instead and say so in the report. Then: `cargo test -p geode-shell 2>&1 | grep -E "^test result" | awk '{p+=$4; f+=$6} END {print "passed="p" failed="f}'` → `failed=0` (the existing as-of tests must still pass — the presets list and `enter` are untouched).

- [ ] **Step 6: fmt, clippy, commit**

```bash
git add crates/geode-shell/src/shell/mod.rs crates/geode-shell/src/shell/asof_view.rs crates/geode-shell/src/shell/tests/asof.rs
git commit -m "shell: a calendar pane in the as-of dialog — a day click writes the date, typing mirrors back, live hides it"
```

---

### Task 4: Harness entries, count, spec as-built, workspace verification

**Files:**
- Modify: `scripts/mutation-check.sh` (three entries after the last `asof`/`as-of` entry — grep `run_mutation "as-of` / `"asof` to find the section), `CLAUDE.md:85` (count), the spec §5.2 (append "As built")

- [ ] **Step 1: Harness entries** (copy each `from` anchor from the committed file byte for byte; **commit first**)

```zsh
# As-of date picker (2026-09-18): a bare YYYY-MM-DD is the END of that
# day — the newest generation of the day — not its start. Resolving to
# midnight would make "2026-09-08" show the previous evening's last
# publish, and every other assertion on the parse stays green.
run_mutation "asof: a bare date resolves to the end of the day" \
  crates/geode-core/src/query.rs \
  '        return resolve_local(d, END_OF_DAY, text);' \
  '        return resolve_local(d, NaiveTime::MIN, text);' \
  geode-core \
  a_bare_date_resolves_to_the_end_of_that_local_day

# A day click must keep a typed time: dropping it silently turns
# "14:05 on the 9th" into "end of the 9th".
run_mutation "asof: a day click keeps the typed time" \
  crates/geode-shell/src/shell/asof_view.rs \
  '    if keeps_time {' \
  '    if false {' \
  geode-shell \
  compose_keeps_a_typed_time_and_replaces_or_adds_the_date

# The calendar is hidden under `live`: painting it there invites a click
# that silently turns "now" into a historical day.
run_mutation "asof: the calendar hides under live" \
  crates/geode-shell/src/shell/asof_view.rs \
  '    !text.trim().eq_ignore_ascii_case("live")' \
  '    true' \
  geode-shell \
  the_calendar_hides_only_under_live
```

Run `zsh scripts/mutation-check.sh --anchors-only` (exit 0), then each by name substring (`"asof: a bare date"`, `"asof: a day click"`, `"asof: the calendar"`) → `caught`. Update CLAUDE.md line 85's count to `grep -c "^run_mutation " scripts/mutation-check.sh`.

- [ ] **Step 2: Spec §5.2 as-built**

Append to §5.2:

```markdown
**As built (2026-09-18):** `parse_as_of` (`geode-core::query`) takes
`YYYY-MM-DD` (→ `END_OF_DAY`, 23:59:59 local — whole seconds, not the
paragraph's `.999`: every displayed time is whole-second and generations
are stamped to the second), `YYYY-MM-DD HH:MM` and `YYYY-MM-DD HH:MM:SS`
(that local instant), beside the existing `HH:MM[:SS]`-today and RFC 3339
forms; a DST gap or overlap is still the one `Err`. The dialog's calendar
is one gpui-kit `CalendarState` entity on `ShellView` (built once, like
`dialog_input`; a focus handle per open would leak), painted small beside
the presets and hidden while the field reads `live`; a day click reaches
the field through `asof_view::on_calendar_selected` — `compose_with_date`
keeps a typed time (`14:05` → `2026-09-08 14:05`) and otherwise writes the
bare date — then re-resolves at once (`set_value` emits no `Change`) and
refocuses the field, so the calendar never holds the keyboard; typing
mirrors the parsed day back onto the calendar (`calendar_date`: the
resolved instant's local day, else today). Window tests drive the
calendar entity's own `activate_date`, the call its day cell makes. Display
check pending: the calendar's size beside the presets and its theme
colours on a real window.
```

- [ ] **Step 3: Whole-workspace verification and commit**

Run: `cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings 2>&1 | tail -1 && cargo test --workspace 2>&1 | grep -E "^test result|FAILED" | awk '/^test result/ {p+=$4; f+=$6} /FAILED/ {print} END {print "passed="p" failed="f}' && cargo bench --workspace --no-run 2>&1 | tail -1 && cargo check -p geode-shell --features test-support --all-targets 2>&1 | tail -1 && zsh scripts/mutation-check.sh --anchors-only | tail -1`
Expected: clean / `Finished` / `failed=0` / `Finished` / `Finished` / `0 stale, 0 ambiguous`.

```bash
git add scripts/mutation-check.sh CLAUDE.md docs/superpowers/specs/2026-09-17-geode-gpui-kit-upgrade-and-adoption-design.md
git commit -m "docs + harness: as-of date picker as built; three harness entries"
```
