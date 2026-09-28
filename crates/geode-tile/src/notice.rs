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
