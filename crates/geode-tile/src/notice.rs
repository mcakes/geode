//! A tile's notice: one line of text in one of three tones, painted in theme
//! tokens. A tile with several notice slots (the pricer's pricing, view and
//! save notices; market-data's notice and upload error) decides which one
//! shows; this door paints the winner, so a tone is one color in every tile.
//!
//! A warning or danger notice can be dismissed: a click on it, or `escape`
//! once nothing else in the tile answers it, and both do the same thing to
//! the same notice. A transient one-shot notice (a refusal or advisory a
//! key set in the module's transient slot) is cleared from its slot, so
//! whatever that slot masked shows and repeating the key says it again. A
//! standing notice (an error, a standing refusal, a derived or save notice
//! the module would report again) is hidden while the tile keeps reporting
//! it: [`Dismissals`] is that state, one per tile, owned by the module.
//! Identity is the notice's text and tone: the module's standing fields are
//! never cleared, the render filters through [`Dismissals::visible`], and
//! the module prunes at the seam where it prepares its notices
//! ([`Dismissals::prune`]), so a notice that stops and returns shows again.
//! Without the prune a derived notice (recomputed on every rebuild) would
//! stay hidden forever; without the dismissal living outside the module's
//! fields it would reappear on the next rebuild. Which slot a notice came
//! from is the module's knowledge, so the door takes the module's handler
//! ([`on_dismiss_with`]). `Status` (`loading…`) is never dismissed.

use std::rc::Rc;

use geode_shell::shell::chip::{self, chip_paint};
use geode_shell::shell::control::{self, PointerStates as _};
use geode_shell::tips;
use gpui::prelude::*;
use gpui::{
    App, Context, Div, ElementId, Entity, Hsla, MouseButton, SharedString, Stateful, Window, div,
};
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

impl Tone {
    /// Whether a notice in this tone can be dismissed: warning and danger
    /// can, a status (progress with nothing wrong) cannot.
    pub fn dismissable(self) -> bool {
        matches!(self, Tone::Warning | Tone::Danger)
    }
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

    pub fn dismissable(&self) -> bool {
        self.tone.dismissable()
    }
}

/// The notices the trader dismissed in one tile, each hidden while the
/// tile keeps reporting it. Render only reads it ([`Self::visible`]); a
/// click or `escape` adds to it ([`Self::dismiss`], [`Self::dismiss_all`]);
/// the module's notice-preparation seam forgets what is no longer reported
/// ([`Self::prune`]). Small: a tile reports a handful of notices at most.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Dismissals {
    hidden: Vec<Notice>,
}

impl Dismissals {
    /// Hide `notice`. `false` for a status notice (never hidden) and for
    /// one already hidden: nothing changed, so nothing repaints.
    pub fn dismiss(&mut self, notice: &Notice) -> bool {
        if !notice.dismissable() || self.hidden.contains(notice) {
            return false;
        }
        self.hidden.push(notice.clone());
        true
    }

    /// `escape`'s answer: hide every warning and danger notice in
    /// `reported` that still shows. Whether any was hidden, so a tile
    /// consumes the key only when it did something.
    pub fn dismiss_all<'a>(&mut self, reported: impl IntoIterator<Item = &'a Notice>) -> bool {
        let mut any = false;
        for n in reported {
            any |= self.dismiss(n);
        }
        any
    }

    /// Whether `notice` paints: everything but a hidden notice.
    pub fn shows(&self, notice: &Notice) -> bool {
        !self.hidden.contains(notice)
    }

    /// `reported` less the hidden notices, in order: what the tile paints.
    pub fn visible(&self, reported: impl IntoIterator<Item = Notice>) -> Vec<Notice> {
        reported.into_iter().filter(|n| self.shows(n)).collect()
    }

    /// Forget every hidden notice not in `reported`, the tile's notices as
    /// its seam just prepared them: one that stopped being reported shows
    /// again the next time it is. Returns whether anything was forgotten.
    /// Never changes what paints (a forgotten notice was not reported), so
    /// the seam needs no notify of its own.
    pub fn prune<'a>(&mut self, reported: impl IntoIterator<Item = &'a Notice>) -> bool {
        if self.hidden.is_empty() {
            return false;
        }
        let reported: Vec<&Notice> = reported.into_iter().collect();
        let before = self.hidden.len();
        self.hidden.retain(|h| reported.contains(&h));
        self.hidden.len() != before
    }

    /// Whether anything is hidden.
    pub fn is_empty(&self) -> bool {
        self.hidden.is_empty()
    }
}

/// What a press on a dismissable notice runs: the module's own
/// [`Dismissals::dismiss`] and repaint. The door cannot reach the module's
/// state, so the module hands this in ([`on_dismiss`]).
pub type OnDismiss = Rc<dyn Fn(&Notice, &mut Window, &mut App)>;

/// The press for a tile `entity` whose notices are all standing, whose
/// dismissals `field` reaches: hide the pressed notice and repaint the tile
/// when that changed anything.
pub fn on_dismiss<T: 'static>(
    entity: &Entity<T>,
    field: fn(&mut T) -> &mut Dismissals,
) -> OnDismiss {
    let tile = entity.downgrade();
    Rc::new(move |notice, _, cx| {
        let _ = tile.update(cx, |t, cx| {
            if field(t).dismiss(notice) {
                cx.notify();
            }
        });
    })
}

/// The press for a tile `entity` with a transient slot: `handler` is the
/// module's own dismissal, the one its `escape` runs per notice — it clears
/// a transient one-shot notice from its slot (what `escape` does to it,
/// revealing whatever that slot masked) and hides a standing one through
/// its [`Dismissals`]. It returns whether anything changed; the tile then
/// repaints. Both hold the tile weakly, so a painted notice never keeps a
/// closed tile.
pub fn on_dismiss_with<T: 'static>(
    entity: &Entity<T>,
    handler: fn(&mut T, &Notice, &mut Context<T>) -> bool,
) -> OnDismiss {
    let tile = entity.downgrade();
    Rc::new(move |notice, _, cx| {
        let _ = tile.update(cx, |t, cx| {
            if handler(t, notice, cx) {
                cx.notify();
            }
        });
    })
}

/// A stable element key for `notices[i]` among its siblings, derived from
/// the notice itself (its text and tone) and, for a repeat, how many equal
/// notices precede it — never its position. Keyed by position, dismissing
/// notice 0 would hand notice 1's id, and with it gpui's per-id tooltip and
/// pressed state, to the notice that moved into its place.
pub fn element_key(notices: &[Notice], i: usize) -> u64 {
    use std::hash::{Hash as _, Hasher as _};
    let notice = &notices[i];
    let repeat = notices[..i].iter().filter(|n| *n == notice).count();
    // SipHash with fixed keys: the same notice keys the same on every paint.
    let mut h = std::collections::hash_map::DefaultHasher::new();
    notice.text.as_ref().hash(&mut h);
    (notice.tone as u8).hash(&mut h);
    repeat.hash(&mut h);
    h.finish()
}

/// The dismissable notice's tooltip detail: both routes, the key spelled
/// `escape` as every binding spells it so its chip reads ⎋.
pub const DISMISS_HINT: &str = "click or `escape` dismisses";

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

/// A notice in a width-bound slot (a header cluster): one line, cut with an
/// ellipsis where the slot is narrower than the text, and the whole text in
/// its tooltip. Unbounded, a long error either wraps out of a one-line strip
/// or pushes its neighbours off the tile; cut without the tooltip, the rest
/// of it is unreadable. `id` must be unique among the slot's siblings. With
/// `on_dismiss`, a warning or danger notice is also [`dismissable`].
pub fn truncated(
    notice: &Notice,
    id: impl Into<ElementId>,
    tip_selector: SharedString,
    on_dismiss: Option<&OnDismiss>,
    theme: &Theme,
) -> Stateful<Div> {
    let el = render(notice, theme).id(id).min_w_0().truncate();
    if armed(notice, on_dismiss).is_some() {
        dismissable(el, notice, on_dismiss, tip_selector, theme)
    } else {
        el.tooltip(tips::tip_with(
            tip_selector,
            notice.text.clone(),
            None,
            None,
        ))
    }
}

/// The one gate for a press: the dismiss, when there is one and the
/// notice is a warning or danger. A status notice is never armed.
fn armed<'a>(notice: &Notice, on_dismiss: Option<&'a OnDismiss>) -> Option<&'a OnDismiss> {
    on_dismiss.filter(|_| notice.dismissable())
}

/// `el`, a painted notice, made dismissable when `on_dismiss` is given and
/// the notice is a warning or danger: the control door's bare hover and
/// pressed states, a tooltip with the whole text and [`DISMISS_HINT`], and
/// a left press that runs `on_dismiss` and stops there (no tile focus, no
/// drag, no header handler beneath it). Otherwise `el` unchanged: a status
/// notice, or one with no dismiss, takes no affordance and no press.
pub fn dismissable(
    el: Stateful<Div>,
    notice: &Notice,
    on_dismiss: Option<&OnDismiss>,
    tip_selector: SharedString,
    theme: &Theme,
) -> Stateful<Div> {
    let Some(on_dismiss) = armed(notice, on_dismiss).cloned() else {
        return el;
    };
    let pressed = notice.clone();
    el.rounded(theme.radius_tokens().sm)
        .pointer_states(control::paint(
            theme,
            control::Rest::Bare,
            theme.background,
            color(notice.tone, theme),
        ))
        .tooltip(tips::tip_with(
            tip_selector,
            notice.text.clone(),
            None,
            Some(SharedString::new_static(DISMISS_HINT)),
        ))
        .on_mouse_down(MouseButton::Left, move |_, window, cx| {
            cx.stop_propagation();
            window.prevent_default();
            on_dismiss(&pressed, window, cx);
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{Context, IntoElement, Render, TestAppContext, Window};
    use gpui_component::ActiveTheme as _;

    fn reported() -> Vec<Notice> {
        vec![
            Notice::status("loading\u{2026}"),
            Notice::warning("not saved"),
            Notice::danger("query failed"),
        ]
    }

    /// A notice keeps its key when one before it is dismissed; equal
    /// notices get distinct keys, the same on every paint.
    #[test]
    fn a_notice_key_is_its_own_not_its_position() {
        let a = Notice::warning("not saved");
        let b = Notice::danger("query failed");
        let both = [a.clone(), b.clone()];
        let alone = [b.clone()];
        assert_eq!(element_key(&both, 1), element_key(&alone, 0));
        assert_ne!(element_key(&both, 0), element_key(&both, 1));
        let twice = [a.clone(), a.clone()];
        assert_ne!(element_key(&twice, 0), element_key(&twice, 1));
        assert_eq!(element_key(&twice, 1), element_key(&[a.clone(), a], 1));
        assert_ne!(
            element_key(&[Notice::warning("x")], 0),
            element_key(&[Notice::danger("x")], 0),
            "tone is part of identity"
        );
    }

    #[test]
    fn only_warning_and_danger_are_dismissable() {
        assert!(!Tone::Status.dismissable());
        assert!(Tone::Warning.dismissable());
        assert!(Tone::Danger.dismissable());
    }

    #[test]
    fn a_dismissed_notice_is_filtered_and_a_status_never_is() {
        let mut d = Dismissals::default();
        assert!(
            !d.dismiss(&Notice::status("loading\u{2026}")),
            "status: refused"
        );
        assert!(d.is_empty());
        assert!(d.dismiss(&Notice::danger("query failed")));
        assert!(
            !d.dismiss(&Notice::danger("query failed")),
            "already hidden"
        );
        assert_eq!(
            d.visible(reported()),
            [
                Notice::status("loading\u{2026}"),
                Notice::warning("not saved")
            ]
        );
    }

    /// Identity is text AND tone: the same words in another tone, or other
    /// words in the same tone, are another notice.
    #[test]
    fn identity_is_text_and_tone() {
        let mut d = Dismissals::default();
        d.dismiss(&Notice::warning("not saved"));
        assert!(d.shows(&Notice::danger("not saved")));
        assert!(d.shows(&Notice::warning("not saved: disk full")));
        assert!(!d.shows(&Notice::warning("not saved")));
    }

    #[test]
    fn dismiss_all_hides_every_showing_warning_and_danger_once() {
        let mut d = Dismissals::default();
        assert!(d.dismiss_all(&reported()));
        assert_eq!(d.visible(reported()), [Notice::status("loading\u{2026}")]);
        assert!(!d.dismiss_all(&reported()), "nothing left to hide");
        assert!(!Dismissals::default().dismiss_all(&[Notice::status("loading")]));
        assert!(!Dismissals::default().dismiss_all(&[]));
    }

    /// Hidden until it changes: kept while reported, forgotten once a
    /// preparation no longer reports it, so its return shows.
    #[test]
    fn a_notice_that_stops_and_returns_shows_again() {
        let mut d = Dismissals::default();
        let failed = Notice::danger("query failed");
        d.dismiss(&failed);
        assert!(!d.prune(&reported()), "still reported: kept");
        assert!(!d.shows(&failed));
        assert!(
            d.prune(&[Notice::warning("not saved")]),
            "stopped: forgotten"
        );
        assert!(d.shows(&failed));
        assert_eq!(d.visible(reported()), reported(), "back: shows");
        assert!(!d.prune(&[]), "nothing hidden: nothing to forget");
    }

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
