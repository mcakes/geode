//! Reusable vim-style list navigation core.
//!
//! A pure state machine, no gpui: feed it [`Keystroke`]s, get back a
//! resolved [`NavCommand`] (or `Pending`/`NotNav`) plus a separate pure
//! [`apply`] function that turns a command into a new clamped selection
//! index. Deliberately reusable beyond the keybinding dialog (Part B, this
//! plan) — the future blotter module is expected to drive its row selection
//! through this exact same `VimListNav` + `apply` pair rather than
//! hand-rolling its own vim subset; keep this module free of anything
//! specific to the keybinding dialog's own list contents.
//!
//! ## Bindings
//!
//! This is this app's own vim-flavored subset, not a literal port of vim's
//! own semantics — several bindings are deliberately redefined:
//!
//! - `j` / `k` — move down/up by 1, multiplied by an optional multi-digit
//!   count prefix typed first (`5j`, `12j`, `3k`). A bare ±1 wraps, a
//!   counted one clamps: see [`apply`].
//! - `ctrl+d` / `ctrl+u` — move by a fixed ±5. **Not** vim's half-page
//!   scroll (which scrolls by half the viewport height, a quantity this
//!   pure state machine has no notion of) — a fixed offset, this app's own
//!   choice.
//! - `ctrl+f` / `ctrl+b` — move by a fixed ±10. Likewise **not** vim's
//!   full-page scroll; same reasoning as `ctrl+d`/`ctrl+u`.
//!
//! These two step sizes are a **general navigation convention**, not a
//! dialog-only one (user ruling 2026-09-11): every list surface that
//! offers `ctrl+d`/`ctrl+u` also offers `ctrl+f`/`ctrl+b` at ±10 — the
//! dialogs through this module, the blotter and diagnostics tiles through
//! their own `page_down`/`page_up` (±5) and `page_down_full`/`page_up_full`
//! (±10) actions in `defaults::BUILTIN_KEYMAP`, with `pagedown`/`pageup`
//! as aliases of the ±10 pair (`listfilter`'s reasoning: free keys, and
//! what a hand reaching for "a screenful" finds first). The same grammar
//! (user ruling 2026-09-12) gives a surface with *columns* `^`/`$` for
//! the first and last column, beside `home`/`end` — the blotter's
//! `first_col`/`last_col` today; any later columnar tile binds the same
//! four keys to the same pair. This module has no column axis and so
//! carries none of it; the convention lives here only so it is written
//! down once, next to the row grammar it extends.
//! - `g` `g` (the bare `g` key, pressed twice) — jump to the top of the
//!   list ([`NavCommand::Top`]).
//! - `shift+g` (vim's `G`) — jump to the bottom of the list
//!   ([`NavCommand::Bottom`]).
//!
//! A count prefix applies **only** to `j`/`k` (the caller's own spec: named
//! explicitly for those two keys and nothing else) — `ctrl+d/u/f/b` and
//! `shift+g` always move by their fixed amount regardless of any pending
//! count, and pressing one of them *while* a count is pending cancels the
//! whole pending gesture (see the state-machine rules below) rather than
//! silently dropping just the count.
//!
//! ## State machine rules
//!
//! `VimListNav` holds at most one kind of pending state at a time — either
//! a pending multi-digit count, or a pending bare `g` (never both at once,
//! since a digit that arrives while `g` is pending is itself "anything
//! non-g" and clears the pending `g` rather than starting a count that
//! somehow combines with it):
//!
//! - While a `g` is pending: another bare `g` completes `Top`; *any other
//!   keystroke* (digit included) clears the pending `g` and resolves to
//!   [`NavResult::NotNav`] for that keystroke — this app defines `g` only
//!   as the two-press `gg`, never as a prefix combined with anything else
//!   (e.g. no `gj`).
//! - While a count is pending: another digit extends it; `j`/`k` consumes
//!   it (multiplying the move) and clears it; *any other keystroke*
//!   (`ctrl+d`, `shift+g`, a bare `g`, or anything unrecognized) clears the
//!   pending count and resolves to `NotNav` for that keystroke too — count
//!   only ever combines with `j`/`k`, so anything else aborts the whole
//!   gesture rather than firing that key's own un-countable behavior.
//! - With nothing pending: each binding above resolves directly; a digit or
//!   a bare `g` starts a new pending gesture ([`NavResult::Pending`]);
//!   anything else is `NotNav` with nothing to clear.

use crate::keymap::{Keystroke, Modifiers};

/// A resolved navigation command: how far (and which direction) to move the
/// selection, or a jump to an end of the list. `Move` is signed and already
/// count-multiplied by the time it comes out of [`VimListNav::press`] —
/// positive moves down (toward higher indices), negative moves up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NavCommand {
    Move(i64),
    Top,
    Bottom,
}

/// The outcome of feeding one [`Keystroke`] to [`VimListNav::press`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NavResult {
    /// A complete navigation command was resolved.
    Command(NavCommand),
    /// Part of a multi-keystroke gesture (a count digit, or the first `g`
    /// of `gg`) — state carries forward; [`VimListNav::pending_display`]
    /// gives a status-line hint for it.
    Pending,
    /// Not part of this navigation vocabulary at all. Any pending count or
    /// `g` has already been cleared as a side effect (see the module doc's
    /// state-machine rules) — the caller is free to route this keystroke
    /// to whatever else handles list keys.
    NotNav,
}

const SHIFT: Modifiers = Modifiers {
    ctrl: false,
    alt: false,
    shift: true,
    cmd: false,
};

fn is_plain(ks: &Keystroke, key: &str) -> bool {
    ks.mods == Modifiers::NONE && ks.key == key
}

fn is_only_ctrl(ks: &Keystroke, key: &str) -> bool {
    ks.mods == Modifiers::CTRL && ks.key == key
}

fn is_only_shift(ks: &Keystroke, key: &str) -> bool {
    ks.mods == SHIFT && ks.key == key
}

/// Any bare digit `0`-`9` (including a leading `0`) starts/extends a count.
/// Real vim treats a leading `0` specially — with no count pending yet, `0`
/// is its own motion (start of line), not the first digit of a count — but
/// this vocabulary has no such motion to conflict with (no `apply` command
/// operates on anything but the whole list), so there's nothing to lose by
/// treating `0` uniformly with every other digit here: a lone `0` press just
/// starts a pending count of `0`, and `0` immediately followed by `j`/`k`
/// resolves to `Move(0)` — a harmless no-op move, never a crash or a
/// mis-parsed gesture. Deliberate simplification, not an oversight.
fn as_digit(ks: &Keystroke) -> Option<u64> {
    if ks.mods != Modifiers::NONE {
        return None;
    }
    let mut chars = ks.key.chars();
    let c = chars.next()?;
    if chars.next().is_some() {
        return None;
    }
    c.to_digit(10).map(u64::from)
}

/// Pending-state accumulator for [`press`](Self::press). See the module doc
/// for the exact rules; the fields here are deliberately private — the only
/// public window onto pending state is [`pending_display`](Self::
/// pending_display).
#[derive(Debug, Default)]
pub struct VimListNav {
    /// A count prefix currently being typed (`5`, then `12` on a second `2`
    /// press, etc.). Mutually exclusive with `pending_g` — see module doc.
    count: Option<u64>,
    /// True right after a bare `g` press, waiting for a second `g` to
    /// complete `gg` (Top).
    pending_g: bool,
}

impl VimListNav {
    pub fn new() -> Self {
        Self::default()
    }

    /// Clear any pending count or `g` without producing a command — for a
    /// caller that wants to abandon an in-progress gesture on some external
    /// event (e.g. the list losing focus, or the dialog closing).
    pub fn cancel(&mut self) {
        self.count = None;
        self.pending_g = false;
    }

    /// A short status-line hint for the currently pending gesture, if any:
    /// the digits typed so far (`"12"`), or `"g"` while waiting for the
    /// second `g`. `None` when nothing is pending.
    pub fn pending_display(&self) -> Option<String> {
        if self.pending_g {
            Some("g".to_string())
        } else {
            self.count.map(|n| n.to_string())
        }
    }

    /// Feed one keystroke through the state machine. See the module doc for
    /// the full rule set.
    pub fn press(&mut self, ks: &Keystroke) -> NavResult {
        if self.pending_g {
            self.pending_g = false;
            return if is_plain(ks, "g") {
                NavResult::Command(NavCommand::Top)
            } else {
                // Any other key aborts the gg gesture rather than doing
                // anything else with it (module doc: no gg-combined-with-
                // anything-else bindings exist in this app's subset).
                self.count = None;
                NavResult::NotNav
            };
        }

        if let Some(count) = self.count {
            if let Some(digit) = as_digit(ks) {
                self.count = Some(count * 10 + digit);
                return NavResult::Pending;
            }
            if is_plain(ks, "j") {
                self.count = None;
                return NavResult::Command(NavCommand::Move(count as i64));
            }
            if is_plain(ks, "k") {
                self.count = None;
                return NavResult::Command(NavCommand::Move(-(count as i64)));
            }
            // A digit followed by anything else (ctrl+d/u/f/b, shift+g, a
            // bare g, or anything unrecognized) aborts the whole pending
            // count rather than falling through to that key's normal,
            // un-countable behavior — see module doc.
            self.count = None;
            return NavResult::NotNav;
        }

        if is_plain(ks, "j") {
            return NavResult::Command(NavCommand::Move(1));
        }
        if is_plain(ks, "k") {
            return NavResult::Command(NavCommand::Move(-1));
        }
        if is_only_ctrl(ks, "d") {
            return NavResult::Command(NavCommand::Move(5));
        }
        if is_only_ctrl(ks, "u") {
            return NavResult::Command(NavCommand::Move(-5));
        }
        if is_only_ctrl(ks, "f") {
            return NavResult::Command(NavCommand::Move(10));
        }
        if is_only_ctrl(ks, "b") {
            return NavResult::Command(NavCommand::Move(-10));
        }
        if is_only_shift(ks, "g") {
            return NavResult::Command(NavCommand::Bottom);
        }
        if is_plain(ks, "g") {
            self.pending_g = true;
            return NavResult::Pending;
        }
        if let Some(digit) = as_digit(ks) {
            self.count = Some(digit);
            return NavResult::Pending;
        }

        NavResult::NotNav
    }
}

/// Apply a resolved [`NavCommand`] to a `selected` index against a list of
/// `len` items. **A bare ±1 wraps at both ends; every larger step clamps**
/// (interaction-model spec §20.5, user ruling 2026-09-14): `j` at the
/// last row lands on the first, `ctrl+d` at the last row stays, and a
/// counted `2j` — which `Cursor::move_rows` multiplies into the delta
/// before calling here — clamps like any other multi-row step, with no
/// special case for a count of one. `Top`/`Bottom` are absolute. An
/// empty list (`len == 0`) always yields `0`, regardless of the command.
///
/// Two axes deliberately do NOT take this rule and call
/// [`apply_clamped`] instead: a column axis (`h`/`l` — the ruling was
/// about rows) and a blotter in visual mode (a wrapping `j` at the bottom
/// would put the cursor above the anchor and invert the selection).
pub fn apply(selected: usize, len: usize, cmd: NavCommand) -> usize {
    match cmd {
        NavCommand::Move(delta) if delta.abs() == 1 && len > 0 => {
            let len = len as i64;
            ((selected as i64 + delta).rem_euclid(len)) as usize
        }
        _ => apply_clamped(selected, len, cmd),
    }
}

/// [`apply`] without the single-step wrap: clamped to `0..len` at both
/// ends whatever the delta. The column axis and visual mode use this.
pub fn apply_clamped(selected: usize, len: usize, cmd: NavCommand) -> usize {
    if len == 0 {
        return 0;
    }
    let max = (len - 1) as i64;
    match cmd {
        NavCommand::Move(delta) => (selected as i64 + delta).clamp(0, max) as usize,
        NavCommand::Top => 0,
        NavCommand::Bottom => len - 1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(key: &str) -> Keystroke {
        Keystroke {
            mods: Modifiers::NONE,
            key: key.to_string(),
        }
    }

    fn ctrl(key: &str) -> Keystroke {
        Keystroke {
            mods: Modifiers::CTRL,
            key: key.to_string(),
        }
    }

    fn shift(key: &str) -> Keystroke {
        Keystroke {
            mods: SHIFT,
            key: key.to_string(),
        }
    }

    // --- individual bindings ---------------------------------------------

    #[test]
    fn j_moves_down_by_one() {
        let mut nav = VimListNav::new();
        assert_eq!(
            nav.press(&key("j")),
            NavResult::Command(NavCommand::Move(1))
        );
    }

    #[test]
    fn k_moves_up_by_one() {
        let mut nav = VimListNav::new();
        assert_eq!(
            nav.press(&key("k")),
            NavResult::Command(NavCommand::Move(-1))
        );
    }

    #[test]
    fn ctrl_d_moves_down_by_five() {
        let mut nav = VimListNav::new();
        assert_eq!(
            nav.press(&ctrl("d")),
            NavResult::Command(NavCommand::Move(5))
        );
    }

    #[test]
    fn ctrl_u_moves_up_by_five() {
        let mut nav = VimListNav::new();
        assert_eq!(
            nav.press(&ctrl("u")),
            NavResult::Command(NavCommand::Move(-5))
        );
    }

    #[test]
    fn ctrl_f_moves_down_by_ten() {
        let mut nav = VimListNav::new();
        assert_eq!(
            nav.press(&ctrl("f")),
            NavResult::Command(NavCommand::Move(10))
        );
    }

    #[test]
    fn ctrl_b_moves_up_by_ten() {
        let mut nav = VimListNav::new();
        assert_eq!(
            nav.press(&ctrl("b")),
            NavResult::Command(NavCommand::Move(-10))
        );
    }

    #[test]
    fn gg_jumps_to_top() {
        let mut nav = VimListNav::new();
        assert_eq!(nav.press(&key("g")), NavResult::Pending);
        assert_eq!(nav.press(&key("g")), NavResult::Command(NavCommand::Top));
    }

    #[test]
    fn shift_g_jumps_to_bottom() {
        let mut nav = VimListNav::new();
        assert_eq!(
            nav.press(&shift("g")),
            NavResult::Command(NavCommand::Bottom)
        );
    }

    // --- multi-digit counts -----------------------------------------------

    #[test]
    fn count_prefix_multiplies_j() {
        let mut nav = VimListNav::new();
        assert_eq!(nav.press(&key("5")), NavResult::Pending);
        assert_eq!(
            nav.press(&key("j")),
            NavResult::Command(NavCommand::Move(5))
        );
    }

    #[test]
    fn count_prefix_multiplies_k() {
        let mut nav = VimListNav::new();
        assert_eq!(nav.press(&key("3")), NavResult::Pending);
        assert_eq!(
            nav.press(&key("k")),
            NavResult::Command(NavCommand::Move(-3))
        );
    }

    #[test]
    fn a_leading_zero_starts_a_count_like_any_other_digit() {
        // Deliberately not vim's own "0 = start of line" motion (see
        // as_digit's doc comment) — 0 is just another count digit here.
        let mut nav = VimListNav::new();
        assert_eq!(nav.press(&key("0")), NavResult::Pending);
        assert_eq!(nav.pending_display(), Some("0".to_string()));
        assert_eq!(
            nav.press(&key("j")),
            NavResult::Command(NavCommand::Move(0)),
            "0j is a harmless no-op move, not a crash or a special motion"
        );
    }

    #[test]
    fn multi_digit_count_accumulates_in_order() {
        let mut nav = VimListNav::new();
        assert_eq!(nav.press(&key("1")), NavResult::Pending);
        assert_eq!(nav.press(&key("2")), NavResult::Pending);
        assert_eq!(
            nav.press(&key("j")),
            NavResult::Command(NavCommand::Move(12))
        );
    }

    #[test]
    fn count_is_cleared_after_being_consumed() {
        let mut nav = VimListNav::new();
        nav.press(&key("5"));
        nav.press(&key("j"));
        // A bare j right after must move by 1, not still be multiplied.
        assert_eq!(
            nav.press(&key("j")),
            NavResult::Command(NavCommand::Move(1))
        );
    }

    // --- count + wrong key clears ------------------------------------------

    #[test]
    fn count_followed_by_unrelated_key_is_notnav_and_clears() {
        let mut nav = VimListNav::new();
        nav.press(&key("5"));
        assert_eq!(nav.press(&key("x")), NavResult::NotNav);
        assert_eq!(nav.pending_display(), None);
        // The cleared count must not leak into the next j.
        assert_eq!(
            nav.press(&key("j")),
            NavResult::Command(NavCommand::Move(1))
        );
    }

    #[test]
    fn count_followed_by_ctrl_d_aborts_rather_than_firing_ctrl_d() {
        let mut nav = VimListNav::new();
        nav.press(&key("5"));
        assert_eq!(
            nav.press(&ctrl("d")),
            NavResult::NotNav,
            "count only combines with j/k; ctrl+d while a count is pending aborts the gesture"
        );
        assert_eq!(nav.pending_display(), None);
    }

    #[test]
    fn count_followed_by_shift_g_aborts_rather_than_jumping_to_bottom() {
        let mut nav = VimListNav::new();
        nav.press(&key("9"));
        assert_eq!(nav.press(&shift("g")), NavResult::NotNav);
        assert_eq!(nav.pending_display(), None);
    }

    #[test]
    fn count_followed_by_bare_g_aborts_rather_than_starting_gg() {
        let mut nav = VimListNav::new();
        nav.press(&key("2"));
        assert_eq!(nav.press(&key("g")), NavResult::NotNav);
        assert_eq!(nav.pending_display(), None);
    }

    // --- g-then-other clears ------------------------------------------------

    #[test]
    fn g_then_j_clears_and_is_notnav() {
        let mut nav = VimListNav::new();
        nav.press(&key("g"));
        assert_eq!(nav.press(&key("j")), NavResult::NotNav);
        assert_eq!(nav.pending_display(), None);
        // Must not still be waiting for a second g.
        assert_eq!(
            nav.press(&key("j")),
            NavResult::Command(NavCommand::Move(1))
        );
    }

    #[test]
    fn g_then_digit_clears_and_is_notnav() {
        let mut nav = VimListNav::new();
        nav.press(&key("g"));
        assert_eq!(nav.press(&key("5")), NavResult::NotNav);
        assert_eq!(nav.pending_display(), None);
    }

    #[test]
    fn g_then_unrelated_key_clears_and_is_notnav() {
        let mut nav = VimListNav::new();
        nav.press(&key("g"));
        assert_eq!(nav.press(&key("x")), NavResult::NotNav);
        assert_eq!(nav.pending_display(), None);
    }

    // --- cancel -------------------------------------------------------------

    #[test]
    fn cancel_clears_a_pending_count() {
        let mut nav = VimListNav::new();
        nav.press(&key("4"));
        nav.cancel();
        assert_eq!(nav.pending_display(), None);
        assert_eq!(
            nav.press(&key("j")),
            NavResult::Command(NavCommand::Move(1))
        );
    }

    #[test]
    fn cancel_clears_a_pending_g() {
        let mut nav = VimListNav::new();
        nav.press(&key("g"));
        nav.cancel();
        assert_eq!(nav.pending_display(), None);
        assert_eq!(nav.press(&key("g")), NavResult::Pending);
    }

    // --- pending_display ------------------------------------------------

    #[test]
    fn pending_display_is_none_with_nothing_pending() {
        let nav = VimListNav::new();
        assert_eq!(nav.pending_display(), None);
    }

    #[test]
    fn pending_display_shows_accumulated_digits() {
        let mut nav = VimListNav::new();
        nav.press(&key("1"));
        assert_eq!(nav.pending_display(), Some("1".to_string()));
        nav.press(&key("2"));
        assert_eq!(nav.pending_display(), Some("12".to_string()));
    }

    #[test]
    fn pending_display_shows_g_while_awaiting_the_second_g() {
        let mut nav = VimListNav::new();
        nav.press(&key("g"));
        assert_eq!(nav.pending_display(), Some("g".to_string()));
    }

    // --- unrelated / not-nav keys ------------------------------------------

    #[test]
    fn unrelated_key_with_nothing_pending_is_notnav() {
        let mut nav = VimListNav::new();
        assert_eq!(nav.press(&key("x")), NavResult::NotNav);
        assert_eq!(nav.press(&key("enter")), NavResult::NotNav);
    }

    // --- apply() clamping ---------------------------------------------------

    #[test]
    fn apply_move_clamps_at_the_bottom() {
        assert_eq!(apply(3, 5, NavCommand::Move(10)), 4);
    }

    #[test]
    fn apply_move_clamps_at_the_top() {
        assert_eq!(apply(1, 5, NavCommand::Move(-10)), 0);
    }

    #[test]
    fn apply_move_within_range() {
        assert_eq!(apply(2, 5, NavCommand::Move(2)), 4);
        assert_eq!(apply(2, 5, NavCommand::Move(-2)), 0);
    }

    #[test]
    fn apply_top_and_bottom() {
        assert_eq!(apply(3, 7, NavCommand::Top), 0);
        assert_eq!(apply(3, 7, NavCommand::Bottom), 6);
    }

    #[test]
    fn apply_on_an_empty_list_is_always_zero() {
        assert_eq!(apply(0, 0, NavCommand::Move(5)), 0);
        assert_eq!(apply(0, 0, NavCommand::Top), 0);
        assert_eq!(apply(0, 0, NavCommand::Bottom), 0);
    }

    #[test]
    fn apply_wraps_a_single_step_at_both_ends() {
        // Spec §20.5: a bare ±1 wraps — the palette's rule, now
        // everyone's.
        assert_eq!(apply(4, 5, NavCommand::Move(1)), 0);
        assert_eq!(apply(0, 5, NavCommand::Move(-1)), 4);
        assert_eq!(
            apply(0, 1, NavCommand::Move(1)),
            0,
            "one row wraps to itself"
        );
    }

    #[test]
    fn apply_clamps_every_larger_step() {
        // ±5 / ±10 (and a counted ±1, which arrives here already
        // multiplied — `Cursor::move_rows`) clamp, never wrap.
        assert_eq!(apply(4, 5, NavCommand::Move(2)), 4);
        assert_eq!(apply(0, 5, NavCommand::Move(-2)), 0);
        assert_eq!(apply(3, 5, NavCommand::Move(10)), 4);
        assert_eq!(apply(1, 5, NavCommand::Move(-10)), 0);
    }

    #[test]
    fn apply_clamped_never_wraps() {
        // The column axis and visual mode use this one.
        assert_eq!(apply_clamped(4, 5, NavCommand::Move(1)), 4);
        assert_eq!(apply_clamped(0, 5, NavCommand::Move(-1)), 0);
        assert_eq!(apply_clamped(0, 0, NavCommand::Move(1)), 0);
    }
}
