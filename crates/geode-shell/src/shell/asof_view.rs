//! The as-of selector (Phase 4a §3.6): a keyed modal in
//! [`keybindings_view`](super::keybindings_view)'s mould — the shared
//! dialog filter field (`ShellView::dialog_input`) doubles as the as-of
//! text field here rather than a query filter, and a list of recent
//! generation times sits underneath it as honest presets: as-of resolves
//! to "the newest generation at or before T", so a generation's own
//! instant is exactly the value that changes what a query sees.
//!
//! ## Architecture
//!
//! [`AsOfState`] — `selected`, `error`, `resolved` — is pure (no `gpui`),
//! stored on `ShellView` as `as_of_dialog: Option<AsOfState>`, exactly
//! like `picker`/`keybindings`/`settings`. Unlike those three, this
//! dialog carries no `query` string of its own to mirror the field into:
//! the field's raw text IS the value being edited, not a filter over
//! something else, so [`on_query_changed`] re-resolves it on every
//! `InputEvent::Change` (the same shared subscription arm those other
//! dialogs use, `shell/mod.rs`) and stores only the *outcome* —
//! `resolved` (a successful parse's instant, for the "→ ..." preview) or
//! `error` (a failed parse's message, shown inline) — for [`build`] to
//! show. The two are mutually exclusive and both `None` while the field
//! is blank (spec: a blank field means "browsing presets", not "typing
//! an instant").
//!
//! [`open`] is the only entry point (`frame::as_of`, `mod+t`) and the
//! only place an `AsOfState` is constructed — nothing survives a
//! close/reopen, the same contract every other modal here keeps.

use std::cell::RefCell;
use std::rc::Rc;

use chrono::{DateTime, Local, NaiveDate, NaiveTime, Utc};
use gpui::prelude::*;
use gpui::{AnyElement, App, Context, Entity, Hsla, MouseButton, Window, div, px};
use gpui_component::{ActiveTheme as _, h_flex, v_flex};

use geode_core::query::{AsOf, parse_as_of};

use crate::frame::Frame;
use crate::keymap::{Keystroke, Modifiers};
use crate::{listfilter, vimnav};

use super::ShellView;
use super::dialog;

// ---------------------------------------------------------------------
// Pure core — no gpui.
// ---------------------------------------------------------------------

/// One formatted preset row: the publish's instant, and its
/// `"14:05:12 · risk / EOD · 3 books"` label.
type PresetRow = (DateTime<Utc>, String);
/// [`AsOfState::presets_cache`]'s value type — named so clippy's
/// `type_complexity` lint doesn't fire on the field declaration.
type PresetsCache = RefCell<Option<(u64, Rc<Vec<PresetRow>>)>>;

/// Persistent state for one open as-of dialog session — the
/// `picker`/`keybindings`/`settings` fields' own contract: no `gpui`
/// types, so every transition here is unit-testable without a window.
#[derive(Debug, Clone, Default)]
pub struct AsOfState {
    /// Index into [`presets`]' own list (the palette's own convention) —
    /// moved by up/down, applied by a bare `enter` when the field is
    /// empty.
    pub selected: usize,
    /// The field's most recent parse failure, or `None` — set by
    /// [`on_query_changed`] (the `InputEvent::Change` subscription) and,
    /// redundantly but harmlessly, by [`handle_key`]'s own `enter` arm on
    /// a failed commit. Mutually exclusive with `resolved`.
    pub error: Option<String>,
    /// The field's most recent successful resolution to an instant —
    /// never set for `"live"`, which has no instant to preview. Mutually
    /// exclusive with `error`.
    pub resolved: Option<DateTime<Utc>>,
    /// Lazy cache for [`cached_presets`] (Phase 4b M9), keyed on
    /// `Frame::versions().data` — the only counter a fresh publish bumps,
    /// and publishes are exactly what `Frame::recent_publishes` (and so
    /// [`presets`]) reflects; every other frame mutation (scope, grouping,
    /// as-of, config) leaves the preset list untouched. `build` (the
    /// render path) used to call `presets` fresh on every paint, allocating
    /// a `Vec` of formatted `String`s for a value that almost always
    /// matches the previous paint's — the same `RefCell<Option<(key,
    /// Rc<..>)>>` shape `Frame::bar_cache` already uses, for the same
    /// reason.
    presets_cache: PresetsCache,
}

/// The frame's recent publishes as as-of presets (spec §3.6): newest
/// first (`Frame::recent_publishes` is already ordered that way), each
/// labelled `"14:05:12 · risk / EOD · 3 books"` in local time.
fn presets(frame: &Frame) -> Vec<PresetRow> {
    #[cfg(test)]
    tests::PRESETS_CALLS.with(|c| c.set(c.get() + 1));
    frame
        .recent_publishes()
        .iter()
        .map(|p| {
            (
                p.at,
                format!(
                    "{} · {} / {} · {} book{}",
                    p.at.with_timezone(&Local).format("%H:%M:%S"),
                    p.dataset,
                    p.batch,
                    p.books,
                    if p.books == 1 { "" } else { "s" }
                ),
            )
        })
        .collect()
}

/// [`presets`], cached on `state.presets_cache` (Phase 4b M9): a call
/// with `frame`'s data version unchanged since the last one returns the
/// exact same `Rc` — a refcount bump, no fresh allocation — rather than
/// rebuilding the whole list. Every call site in this module that used
/// to call `presets` directly from a render/key-handling path (`build`,
/// `handle_key`'s enter/up-down arms) goes through this instead.
fn cached_presets(state: &AsOfState, frame: &Frame) -> Rc<Vec<PresetRow>> {
    let v = frame.versions().data;
    if let Some((cached_v, cached)) = state.presets_cache.borrow().as_ref()
        && *cached_v == v
    {
        return Rc::clone(cached);
    }
    let built = Rc::new(presets(frame));
    *state.presets_cache.borrow_mut() = Some((v, Rc::clone(&built)));
    built
}

/// Resolve the as-of field's raw text (spec §3.6): `"live"` (any case)
/// resolves to [`AsOf::Live`]; anything else delegates to
/// [`parse_as_of`] (`HH:MM`, `HH:MM:SS`, or RFC 3339) — an `Err` carries
/// that parser's own message, shown verbatim under the field.
pub fn resolve_input(text: &str, now: DateTime<Utc>) -> Result<AsOf, String> {
    let trimmed = text.trim();
    if trimmed.eq_ignore_ascii_case("live") {
        return Ok(AsOf::Live);
    }
    parse_as_of(trimmed, now).map(AsOf::At)
}

/// The `InputEvent::Change` handler's pure half (shared dialog-input
/// subscription arm, `shell/mod.rs`): re-resolve the field's current
/// text and store the outcome for [`build`] to show. A blank field
/// (nothing typed, or whitespace only) clears both `error` and
/// `resolved` — see [`AsOfState`]'s own doc comment for why.
pub fn on_query_changed(state: &mut AsOfState, text: &str, now: DateTime<Utc>) {
    if text.trim().is_empty() {
        state.error = None;
        state.resolved = None;
        return;
    }
    match resolve_input(text, now) {
        Ok(AsOf::Live) => {
            state.error = None;
            state.resolved = None;
        }
        Ok(AsOf::At(t)) => {
            state.error = None;
            state.resolved = Some(t);
        }
        Err(e) => {
            state.error = Some(e);
            state.resolved = None;
        }
    }
}

/// The field text a calendar day click yields (spec §5.2, §17 mouse
/// parity): a typed time — `HH:MM` or `HH:MM:SS`, alone or after a date
/// — is kept and the date part becomes `date`; anything else (blank,
/// `live`, an RFC 3339 instant, garbage) becomes the bare date, which
/// [`parse_as_of`] reads as the end of that day.
pub fn compose_with_date(text: &str, date: NaiveDate) -> String {
    let trimmed = text.trim();
    let time_part = trimmed.rsplit_once(' ').map(|(_, t)| t).unwrap_or(trimmed);
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

// ---------------------------------------------------------------------
// gpui shell.
// ---------------------------------------------------------------------

/// Target dialog content width in pixels — the picker's own `WIDTH`
/// (`shell::picker`): a text field plus a short list is the same shape.
const WIDTH: f32 = 480.0;

/// Open the as-of dialog (`frame::as_of`, `mod+t`). A no-op if a modal is
/// already open, mirroring every other `open` here.
pub fn open(view: &mut ShellView, window: &mut Window, cx: &mut Context<ShellView>) {
    if view.modal.is_some() {
        return;
    }
    view.as_of_dialog = Some(AsOfState::default());
    let entity = cx.entity();
    dialog::open_shell_dialog_with_key(
        view,
        window,
        cx,
        "As of",
        move |shell, window, cx| build(shell, &entity, window, cx),
        Some(Rc::new(handle_key)),
        true,
    );
}

/// Replace the frame's as-of with `at` and close the dialog — the enter
/// arm's and a preset row click's shared commit path.
fn commit_at(
    shell: &mut ShellView,
    at: DateTime<Utc>,
    window: &mut Window,
    cx: &mut Context<ShellView>,
) {
    shell.frame.update(cx, |f, cx| {
        if f.set_as_of(AsOf::At(at)) {
            cx.notify();
        }
    });
    shell.close_modal(window, cx);
}

/// Return to live and close the dialog — the `"live"` text arm's own
/// commit path.
fn commit_live(shell: &mut ShellView, window: &mut Window, cx: &mut Context<ShellView>) {
    shell.frame.update(cx, |f, cx| {
        if f.set_as_of(AsOf::Live) {
            cx.notify();
        }
    });
    shell.close_modal(window, cx);
}

/// The [`dialog::ModalKeyHandler`] for this modal.
///
/// `enter`: an empty field commits the preset at `selected`, if any (a
/// silent no-op when the preset list is empty); otherwise
/// [`resolve_input`] on the field's text — `Ok(AsOf::Live)`/`Ok(AsOf::
/// At(t))` commit and close, `Err` stores the message on `state.error`
/// (the modal stays open, same "enter does nothing" contract the field's
/// own inline error already promises).
///
/// Every `listfilter::nav_command` key moves `selected` through
/// `vimnav::apply` (spec §20.5); `escape` claims nothing (falls through to
/// `handle_key_down`'s own modal-closes-on-escape branch), exactly like
/// every other dialog here.
fn handle_key(
    shell: &mut ShellView,
    ks: &Keystroke,
    window: &mut Window,
    cx: &mut Context<ShellView>,
) -> bool {
    if ks.mods == Modifiers::NONE && ks.key == "enter" {
        let text = shell.dialog_input.read(cx).value().to_string();
        let trimmed = text.trim();
        if trimmed.is_empty() {
            // Phase 4b Task 1 fix round 1, MIN-11: one borrow of
            // `as_of_dialog`, not two — `selected` used to be read off a
            // separate `as_ref()` call purely to have a fallback-to-0
            // default outside the `and_then`, which `state.selected`
            // gives for free from inside the single borrow below.
            let at = shell.as_of_dialog.as_ref().and_then(|state| {
                cached_presets(state, shell.frame.read(cx))
                    .get(state.selected)
                    .map(|(at, _)| *at)
            });
            if let Some(at) = at {
                commit_at(shell, at, window, cx);
            }
            return true;
        }
        match resolve_input(trimmed, Utc::now()) {
            Ok(AsOf::Live) => commit_live(shell, window, cx),
            Ok(AsOf::At(t)) => commit_at(shell, t, window, cx),
            Err(msg) => {
                if let Some(state) = shell.as_of_dialog.as_mut() {
                    state.error = Some(msg);
                    state.resolved = None;
                }
                cx.notify();
            }
        }
        return true;
    }
    if let Some(cmd) = listfilter::nav_command(ks) {
        let len = shell
            .as_of_dialog
            .as_ref()
            .map(|state| cached_presets(state, shell.frame.read(cx)).len())
            .unwrap_or(0);
        if let Some(state) = shell.as_of_dialog.as_mut() {
            state.selected = vimnav::apply(state.selected, len, cmd);
        }
        cx.notify();
        return true;
    }
    false
}

/// The `Values`-stage-less body: the shared filter field doubling as the
/// as-of text field, the resolved preview or inline error under it, then
/// the preset list.
fn build(
    shell: &ShellView,
    entity: &Entity<ShellView>,
    _window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let Some(state) = shell.as_of_dialog.as_ref() else {
        return div().into_any_element();
    };
    let theme = cx.theme();
    let primary = theme.primary;
    let muted = theme.muted_foreground;
    let selection = theme.selection;
    let danger = theme.danger;

    let mut column =
        v_flex()
            .gap_2()
            .w(px(WIDTH))
            .child(dialog::filter_row(&shell.dialog_input, None, cx));

    if let Some(t) = state.resolved {
        column = column.child(
            div()
                .text_sm()
                .text_color(muted)
                .debug_selector(|| "as-of-resolved".to_string())
                .child(format!(
                    "→ {}",
                    t.with_timezone(&Local).format("%Y-%m-%d %H:%M:%S %Z")
                )),
        );
    }
    if let Some(err) = &state.error {
        column = column.child(
            div()
                .text_sm()
                .text_color(danger)
                .debug_selector(|| "as-of-error".to_string())
                .child(err.clone()),
        );
    }

    let presets_list = cached_presets(state, shell.frame.read(cx));
    let list = build_presets(
        &presets_list,
        state.selected,
        entity,
        primary,
        muted,
        selection,
    );
    column.child(list).into_any_element()
}

/// The preset list: each row `"14:05:12 · risk / EOD · 3 books"`, the
/// selected row highlighted — the palette's own row styling. A row click
/// commits that instant directly ([`commit_at`]), same as the spec's
/// "selecting one sets as-of to that instant".
fn build_presets(
    presets: &[(DateTime<Utc>, String)],
    selected: usize,
    entity: &Entity<ShellView>,
    primary: Hsla,
    muted: Hsla,
    selection: Hsla,
) -> AnyElement {
    if presets.is_empty() {
        return div()
            .px_2()
            .py_1()
            .text_sm()
            .text_color(muted)
            .child("no recent publishes")
            .into_any_element();
    }
    let mut list = v_flex()
        .id("as-of-presets")
        .w(px(WIDTH))
        .gap_1()
        .debug_selector(|| "as-of-presets".to_string());
    for (position, (at, label)) in presets.iter().enumerate() {
        let is_selected = position == selected;
        let mut row = h_flex().w_full().px_2().py_1().rounded(px(4.));
        if is_selected {
            row = row.bg(selection).text_color(primary);
        }
        let at = *at;
        let entity = entity.clone();
        row = row
            .child(label.clone())
            .debug_selector(move || format!("as-of-preset-{position}"))
            .on_mouse_down(MouseButton::Left, move |_event, window, cx| {
                entity.update(cx, |shell, cx| {
                    commit_at(shell, at, window, cx);
                });
            });
        list = list.child(row);
    }
    list.into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::Publish;
    use chrono::TimeZone;
    use std::cell::Cell;

    // Phase 4b M9: counts real `presets` calls so a test can prove
    // `cached_presets` actually skips rebuilding when nothing relevant
    // changed, rather than only checking the returned value (which would
    // look identical whether or not the cache did its job).
    thread_local! {
        pub(super) static PRESETS_CALLS: Cell<u32> = const { Cell::new(0) };
    }

    fn publish(dataset: &str, batch: &str, books: usize, at: DateTime<Utc>) -> Publish {
        Publish {
            dataset: dataset.to_string(),
            batch: batch.to_string(),
            books,
            at,
        }
    }

    #[test]
    fn presets_are_labeled_and_newest_first() {
        use geode_core::groupings::GroupingSlots;
        use geode_core::scopes::SavedScopes;

        let mut f = Frame::new(GroupingSlots::default(), SavedScopes::new(), None);
        let t0 = Utc.with_ymd_and_hms(2026, 9, 6, 14, 5, 12).unwrap();
        let t1 = t0 + chrono::Duration::seconds(5);
        let t2 = t1 + chrono::Duration::seconds(5);
        f.note_published(publish("risk", "EOD", 3, t0));
        f.note_published(publish("risk", "EOD", 1, t1));
        f.note_published(publish("greeks", "INTRADAY", 5, t2));

        let p = presets(&f);
        assert_eq!(p.len(), 3, "newest first, one row per publish");
        assert_eq!(p[0].0, t2);
        assert_eq!(
            p[0].1,
            format!(
                "{} · greeks / INTRADAY · 5 books",
                t2.with_timezone(&Local).format("%H:%M:%S")
            )
        );
        assert_eq!(p[1].0, t1);
        assert_eq!(
            p[1].1,
            format!(
                "{} · risk / EOD · 1 book",
                t1.with_timezone(&Local).format("%H:%M:%S")
            ),
            "a single book is singular, not '1 books'"
        );
        assert_eq!(p[2].0, t0);
    }

    #[test]
    fn cached_presets_rebuilds_only_when_the_frames_data_version_changes() {
        use geode_core::groupings::GroupingSlots;
        use geode_core::scopes::SavedScopes;

        let mut f = Frame::new(GroupingSlots::default(), SavedScopes::new(), None);
        f.note_published(publish(
            "risk",
            "EOD",
            3,
            Utc.with_ymd_and_hms(2026, 9, 6, 14, 5, 12).unwrap(),
        ));
        let state = AsOfState::default();

        PRESETS_CALLS.with(|c| c.set(0));
        let a = cached_presets(&state, &f);
        let b = cached_presets(&state, &f);
        assert!(Rc::ptr_eq(&a, &b), "the second call must hit the cache");
        assert_eq!(
            PRESETS_CALLS.with(|c| c.get()),
            1,
            "two calls with the frame's data version unchanged must build the list once"
        );

        f.note_published(publish(
            "risk",
            "EOD",
            4,
            Utc.with_ymd_and_hms(2026, 9, 6, 14, 6, 0).unwrap(),
        ));
        let c = cached_presets(&state, &f);
        assert!(
            !Rc::ptr_eq(&a, &c),
            "a new publish must invalidate the cache"
        );
        assert_eq!(PRESETS_CALLS.with(|c| c.get()), 2);
    }

    #[test]
    fn resolve_input_delegates_to_parse_as_of_for_a_clock_time() {
        let now = Utc.with_ymd_and_hms(2026, 9, 6, 16, 0, 0).unwrap();
        // F1 (final fix wave): `HH:MM` resolves on the LOCAL date (spec
        // §3.6, "one clock throughout") — computed independently of
        // `resolve_input`/`parse_as_of` so this holds on any machine's
        // zone, the same pattern `geode_core::query`'s own pinning test
        // uses.
        let today_local = now.with_timezone(&Local).date_naive();
        let expected = Local
            .from_local_datetime(
                &today_local.and_time(chrono::NaiveTime::from_hms_opt(14, 5, 0).unwrap()),
            )
            .unwrap()
            .to_utc();
        assert_eq!(resolve_input("14:05", now), Ok(AsOf::At(expected)));
    }

    #[test]
    fn resolve_input_live_is_case_insensitive() {
        let now = Utc::now();
        assert_eq!(resolve_input("live", now), Ok(AsOf::Live));
        assert_eq!(resolve_input("LIVE", now), Ok(AsOf::Live));
        assert_eq!(resolve_input(" Live ", now), Ok(AsOf::Live));
    }

    #[test]
    fn resolve_input_garbage_returns_the_parsers_message() {
        let now = Utc::now();
        let err = resolve_input("nope", now).unwrap_err();
        assert!(err.contains("HH:MM"), "{err}");
    }

    #[test]
    fn on_query_changed_clears_both_fields_for_a_blank_query() {
        let now = Utc::now();
        let mut state = AsOfState {
            selected: 0,
            error: Some("stale".into()),
            resolved: Some(now),
            ..AsOfState::default()
        };
        on_query_changed(&mut state, "   ", now);
        assert!(state.error.is_none());
        assert!(state.resolved.is_none());
    }

    #[test]
    fn on_query_changed_sets_resolved_on_success_and_error_on_failure() {
        let now = Utc.with_ymd_and_hms(2026, 9, 6, 16, 0, 0).unwrap();
        let mut state = AsOfState::default();
        on_query_changed(&mut state, "14:05", now);
        // F1 (final fix wave): local date, not UTC's — see the sibling
        // test's comment above.
        let today_local = now.with_timezone(&Local).date_naive();
        let expected = Local
            .from_local_datetime(
                &today_local.and_time(chrono::NaiveTime::from_hms_opt(14, 5, 0).unwrap()),
            )
            .unwrap()
            .to_utc();
        assert_eq!(state.resolved, Some(expected));
        assert!(state.error.is_none());

        on_query_changed(&mut state, "not a time", now);
        assert!(state.error.is_some());
        assert!(state.resolved.is_none(), "error and resolved are exclusive");
    }

    fn d(y: i32, m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, day).unwrap()
    }

    #[test]
    fn compose_keeps_a_typed_time_and_replaces_or_adds_the_date() {
        let day = d(2026, 9, 8);
        assert_eq!(compose_with_date("", day), "2026-09-08");
        assert_eq!(compose_with_date("   ", day), "2026-09-08");
        assert_eq!(compose_with_date("14:05", day), "2026-09-08 14:05");
        assert_eq!(compose_with_date("14:05:30", day), "2026-09-08 14:05:30");
        assert_eq!(
            compose_with_date("2026-01-01 09:30", day),
            "2026-09-08 09:30"
        );
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
        let now = Utc::now();
        let mut state = AsOfState::default();
        assert_eq!(
            calendar_date(&state, now),
            now.with_timezone(&Local).date_naive()
        );
        on_query_changed(&mut state, "2026-09-08 14:05", now);
        assert_eq!(calendar_date(&state, now), d(2026, 9, 8));
        on_query_changed(&mut state, "live", now);
        assert_eq!(
            calendar_date(&state, now),
            now.with_timezone(&Local).date_naive()
        );
    }

    #[test]
    fn the_calendar_hides_only_under_live() {
        assert!(shows_calendar(""));
        assert!(shows_calendar("14:05"));
        assert!(shows_calendar("nonsense"));
        assert!(!shows_calendar("live"));
        assert!(!shows_calendar(" LIVE "));
    }
}
