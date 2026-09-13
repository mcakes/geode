//! Named colours (Part 2c spec §2): a config doc of shared colours, each
//! either a hue on a canonical wheel that the active theme transforms, or
//! one of the theme's own semantic tokens. Pure: the caller (the shell's
//! swatches, the blotter's cells) reads the theme's twelve base hues and
//! its token colours into [`Anchors`]/[`Tokens`] and calls [`resolve`];
//! nothing here knows a gpui type. A chart resolves a named colour the
//! same way the blotter does, which is the whole point of the doc.

pub mod oklab;

use crate::config::{Diagnostic, MergedDoc, Severity, check_object_name};
use oklab::{Lch, lab_to_lch, srgb_to_oklab, to_srgb_in_gamut};
use std::collections::BTreeMap;
use std::f32::consts::{PI, TAU};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rgb {
    pub r: f32,
    pub g: f32,
    pub b: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Normal,
    Light,
}

/// The theme colours a definition may name directly (spec §2.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Token {
    Foreground,
    Muted,
    Primary,
    Accent,
    Danger,
    Warning,
    Success,
    Info,
    Chart(u8),
    Bullish,
    Bearish,
}

impl Token {
    pub const ALL: [Token; 15] = [
        Token::Foreground,
        Token::Muted,
        Token::Primary,
        Token::Accent,
        Token::Danger,
        Token::Warning,
        Token::Success,
        Token::Info,
        Token::Chart(1),
        Token::Chart(2),
        Token::Chart(3),
        Token::Chart(4),
        Token::Chart(5),
        Token::Bullish,
        Token::Bearish,
    ];

    pub fn parse(s: &str) -> Option<Token> {
        Token::ALL.into_iter().find(|t| t.name() == s)
    }

    pub fn name(self) -> &'static str {
        match self {
            Token::Foreground => "foreground",
            Token::Muted => "muted",
            Token::Primary => "primary",
            Token::Accent => "accent",
            Token::Danger => "danger",
            Token::Warning => "warning",
            Token::Success => "success",
            Token::Info => "info",
            Token::Chart(1) => "chart.1",
            Token::Chart(2) => "chart.2",
            Token::Chart(3) => "chart.3",
            Token::Chart(4) => "chart.4",
            Token::Chart(_) => "chart.5",
            Token::Bullish => "chart.bullish",
            Token::Bearish => "chart.bearish",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Definition {
    Hue { degrees: f32, tone: Tone },
    Token(Token),
}

impl Definition {
    /// The browse summary: `hue 240`, `hue 210 · light`, `token chart.bullish`.
    pub fn summary(&self) -> String {
        match self {
            Definition::Hue {
                degrees,
                tone: Tone::Normal,
            } => format!("hue {}", *degrees as i64),
            Definition::Hue {
                degrees,
                tone: Tone::Light,
            } => format!("hue {} · light", *degrees as i64),
            Definition::Token(t) => format!("token {}", t.name()),
        }
    }
}

/// A column's `colour` key already spells these two.
pub const RESERVED_NAMES: [&str; 2] = ["none", "sign"];

#[derive(Debug, Clone, Default, PartialEq)]
pub struct NamedColours {
    by_name: BTreeMap<String, Definition>,
}

/// Pushes the "both `hue` and `token`" diagnostic and drops the colour.
/// Its own helper because the message embeds three sets of single quotes
/// around the name, `hue` and `token` — the mutation harness's anchor for
/// this arm calls this one line rather than the fragile quoting inside.
fn refuse_both(diags: &mut Vec<Diagnostic>, at: &dyn Fn(&str) -> String, name: &str) {
    diags.push(Diagnostic {
        severity: Severity::Error,
        layer: None,
        file: None,
        message: format!(
            "colour '{name}': both 'hue' and 'token' — a colour is one or the other; dropped"
        ),
        path: Some(at("")),
    });
}

impl NamedColours {
    pub fn from_doc(doc: &MergedDoc) -> (NamedColours, Vec<Diagnostic>) {
        let mut out = NamedColours::default();
        let mut diags = Vec::new();
        let diag = |severity: Severity, path: String, m: String| Diagnostic {
            severity,
            layer: None,
            file: None,
            message: m,
            path: Some(path),
        };
        for (name, value) in &doc.value {
            if name == "config_version" {
                continue;
            }
            let at = |suffix: &str| {
                if suffix.is_empty() {
                    format!("colours.{name}")
                } else {
                    format!("colours.{name}.{suffix}")
                }
            };
            if RESERVED_NAMES.contains(&name.as_str()) || check_object_name(name).is_err() {
                diags.push(diag(
                    Severity::Error,
                    at(""),
                    format!(
                        "colour '{name}': the name is reserved or not a valid object name — dropped"
                    ),
                ));
                continue;
            }
            let Some(table) = value.as_table() else {
                diags.push(diag(
                    Severity::Error,
                    at(""),
                    format!("colour '{name}': not a table — dropped"),
                ));
                continue;
            };
            let hue = table.get("hue");
            let token = table.get("token");
            let definition = match (hue, token) {
                (Some(_), Some(_)) => {
                    refuse_both(&mut diags, &at, name);
                    continue;
                }
                (None, None) => {
                    diags.push(diag(
                        Severity::Error,
                        at(""),
                        format!("colour '{name}': neither 'hue' nor 'token'; dropped"),
                    ));
                    continue;
                }
                (Some(h), None) => {
                    let Some(degrees) = h.as_float().or_else(|| h.as_integer().map(|i| i as f64))
                    else {
                        diags.push(diag(
                            Severity::Error,
                            at("hue"),
                            format!("colour '{name}': 'hue' must be a number (got {h}); dropped"),
                        ));
                        continue;
                    };
                    if !(0.0..=360.0).contains(&degrees) {
                        diags.push(diag(
                            Severity::Error,
                            at("hue"),
                            format!(
                                "colour '{name}': 'hue' must be 0..360 (got {degrees}); dropped"
                            ),
                        ));
                        continue;
                    }
                    let tone = match table.get("tone").and_then(|v| v.as_str()) {
                        None | Some("normal") => Tone::Normal,
                        Some("light") => Tone::Light,
                        Some(other) => {
                            diags.push(diag(
                                Severity::Warning,
                                at("tone"),
                                format!(
                                    "colour '{name}': 'tone' must be \"normal\" or \"light\" (got {other:?}); using normal"
                                ),
                            ));
                            Tone::Normal
                        }
                    };
                    Definition::Hue {
                        degrees: (degrees % 360.0) as f32,
                        tone,
                    }
                }
                (None, Some(t)) => {
                    if table.get("tone").is_some() {
                        diags.push(diag(
                            Severity::Warning,
                            at("tone"),
                            format!(
                                "colour '{name}': 'tone' has no effect beside 'token'; ignored"
                            ),
                        ));
                    }
                    match t.as_str().and_then(Token::parse) {
                        Some(token) => Definition::Token(token),
                        None => {
                            diags.push(diag(
                                Severity::Error,
                                at("token"),
                                format!("colour '{name}': unknown token {t}; dropped"),
                            ));
                            continue;
                        }
                    }
                }
            };
            out.by_name.insert(name.clone(), definition);
        }
        (out, diags)
    }

    pub fn get(&self, name: &str) -> Option<&Definition> {
        self.by_name.get(name)
    }
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.by_name.keys().map(String::as_str)
    }
    pub fn is_empty(&self) -> bool {
        self.by_name.is_empty()
    }
    pub fn insert(&mut self, name: String, def: Definition) {
        self.by_name.insert(name, def);
    }
}

/// red, yellow, green, cyan, blue, magenta — the canonical wheel (§2.2).
pub const ANCHOR_DEGREES: [f32; 6] = [0.0, 60.0, 120.0, 180.0, 240.0, 300.0];

/// A theme's twelve base hues, in `ANCHOR_DEGREES` order, per tone.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Anchors {
    pub normal: [Rgb; 6],
    pub light: [Rgb; 6],
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Tokens {
    pub foreground: Rgb,
    pub muted: Rgb,
    pub primary: Rgb,
    pub accent: Rgb,
    pub danger: Rgb,
    pub warning: Rgb,
    pub success: Rgb,
    pub info: Rgb,
    pub chart: [Rgb; 5],
    pub bullish: Rgb,
    pub bearish: Rgb,
    /// The theme's own background — not a [`Token`] a `colours.toml`
    /// definition can name (there is no `token = "background"`); it is
    /// the surface [`readable_on`] measures every generated `Definition::Hue`
    /// against.
    pub background: Rgb,
}

impl Tokens {
    pub fn get(&self, token: Token) -> Rgb {
        match token {
            Token::Foreground => self.foreground,
            Token::Muted => self.muted,
            Token::Primary => self.primary,
            Token::Accent => self.accent,
            Token::Danger => self.danger,
            Token::Warning => self.warning,
            Token::Success => self.success,
            Token::Info => self.info,
            Token::Chart(n) => self.chart[(n.clamp(1, 5) - 1) as usize],
            Token::Bullish => self.bullish,
            Token::Bearish => self.bearish,
        }
    }
}

/// The hue's two bracketing anchors in the requested tone, interpolated
/// in OKLCH: lightness and chroma linearly, hue along the shorter arc
/// (§2.2). `t == 0` returns the anchor itself, untouched — this
/// function's own identity guarantee is unconditional and pure, with no
/// theme background in sight. [`resolve`] is what a `hue` definition's
/// outward contract actually lives on: an anchor hue resolves to the
/// theme's own colour exactly — unless that colour is unreadable on the
/// theme's background, in which case only its lightness moves (see
/// [`readable_on`]). The arc/anchor tests below exercise this function
/// directly so that guarantee keeps its old, unconditional meaning.
pub fn interpolate_hue(degrees: f32, tone: Tone, anchors: &Anchors) -> Rgb {
    let ring = match tone {
        Tone::Normal => &anchors.normal,
        Tone::Light => &anchors.light,
    };
    let h = degrees.rem_euclid(360.0);
    let i = ((h / 60.0).floor() as usize) % 6;
    let j = (i + 1) % 6;
    let t = (h - ANCHOR_DEGREES[i]) / 60.0;
    if t <= 0.0 {
        return ring[i];
    }
    let a = lab_to_lch(srgb_to_oklab(ring[i]));
    let b = lab_to_lch(srgb_to_oklab(ring[j]));
    let dh = (b.h - a.h + PI).rem_euclid(TAU) - PI; // the shorter arc
    let lch = Lch {
        l: a.l + (b.l - a.l) * t,
        c: a.c + (b.c - a.c) * t,
        h: (a.h + dh * t).rem_euclid(TAU),
    };
    to_srgb_in_gamut(lch)
}

/// The WCAG contrast ratio every resolved `Definition::Hue` must clear
/// against the theme's background (spec §7), enforced by [`readable_on`].
pub const READABLE_RATIO: f32 = 3.0;

/// Pull `rgb`'s OKLCH lightness toward `toward`'s until it clears
/// `READABLE_RATIO` against `background`, keeping hue and chroma (re-clipped
/// to gamut). The smallest such move, found by bisection over `t` in
/// `0..=1` (16 steps); `rgb` unchanged when it already clears.
pub fn readable_on(rgb: Rgb, background: Rgb, toward: Rgb) -> Rgb {
    if contrast_ratio(rgb, background) >= READABLE_RATIO {
        return rgb;
    }
    let lch = lab_to_lch(srgb_to_oklab(rgb));
    let target_l = lab_to_lch(srgb_to_oklab(toward)).l;
    let at = |t: f32| {
        to_srgb_in_gamut(Lch {
            l: lch.l + (target_l - lch.l) * t,
            c: lch.c,
            h: lch.h,
        })
    };
    let (mut lo, mut hi) = (0.0_f32, 1.0_f32);
    for _ in 0..16 {
        let mid = (lo + hi) / 2.0;
        if contrast_ratio(at(mid), background) >= READABLE_RATIO {
            hi = mid;
        } else {
            lo = mid;
        }
    }
    at(hi)
}

/// A named colour's `hue`/`token` definition, resolved against a theme's
/// [`Anchors`]/[`Tokens`]. §2.2's identity rule: an anchor hue resolves
/// to the theme's own colour exactly — unless that colour is unreadable
/// on the theme's background, in which case only its lightness moves,
/// via [`readable_on`]. `Definition::Token` is never floored: a token
/// names one of the theme author's own deliberate semantic colours
/// (danger, a chart series, …), not a generated point on the hue wheel
/// that might land anywhere — the floor exists to guard the generated
/// case, not to second-guess a theme's own design.
pub fn resolve(def: &Definition, anchors: &Anchors, tokens: &Tokens) -> Rgb {
    match def {
        Definition::Hue { degrees, tone } => readable_on(
            interpolate_hue(*degrees, *tone, anchors),
            tokens.background,
            tokens.foreground,
        ),
        Definition::Token(token) => tokens.get(*token),
    }
}

/// WCAG contrast ratio, `1..=21`.
pub fn contrast_ratio(a: Rgb, b: Rgb) -> f32 {
    let (la, lb) = (oklab::relative_luminance(a), oklab::relative_luminance(b));
    let (hi, lo) = if la > lb { (la, lb) } else { (lb, la) };
    (hi + 0.05) / (lo + 0.05)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{LayerDoc, merge_docs};

    fn doc(text: &str) -> crate::config::MergedDoc {
        merge_docs("colours", &[LayerDoc::builtin("colours", text).unwrap()])
    }
    fn grey(v: f32) -> Rgb {
        Rgb { r: v, g: v, b: v }
    }
    /// Six distinct saturated anchors, light tone brighter.
    fn anchors() -> Anchors {
        let normal = [
            Rgb {
                r: 0.8,
                g: 0.2,
                b: 0.2,
            },
            Rgb {
                r: 0.8,
                g: 0.8,
                b: 0.2,
            },
            Rgb {
                r: 0.2,
                g: 0.8,
                b: 0.2,
            },
            Rgb {
                r: 0.2,
                g: 0.8,
                b: 0.8,
            },
            Rgb {
                r: 0.2,
                g: 0.2,
                b: 0.8,
            },
            Rgb {
                r: 0.8,
                g: 0.2,
                b: 0.8,
            },
        ];
        let light = normal.map(|c| Rgb {
            r: c.r * 0.5 + 0.5,
            g: c.g * 0.5 + 0.5,
            b: c.b * 0.5 + 0.5,
        });
        Anchors { normal, light }
    }
    fn tokens() -> Tokens {
        Tokens {
            foreground: grey(0.9),
            muted: grey(0.6),
            primary: grey(0.5),
            accent: grey(0.4),
            danger: grey(0.3),
            warning: grey(0.35),
            success: grey(0.45),
            info: grey(0.55),
            chart: [grey(0.1), grey(0.2), grey(0.3), grey(0.4), grey(0.5)],
            bullish: Rgb {
                r: 0.0,
                g: 1.0,
                b: 0.0,
            },
            bearish: Rgb {
                r: 1.0,
                g: 0.0,
                b: 0.0,
            },
            background: grey(0.1),
        }
    }

    #[test]
    fn reads_hue_tone_and_token_and_refuses_both_or_neither() {
        let (colours, diags) = NamedColours::from_doc(&doc(
            "[delta]\nhue = 240\n[gamma]\nhue = 210\ntone = \"light\"\n[pnl]\ntoken = \"chart.bullish\"\n\
             [both]\nhue = 1\ntoken = \"danger\"\n[neither]\ntone = \"light\"\n[wrap]\nhue = 360\n",
        ));
        assert_eq!(
            colours.get("delta"),
            Some(&Definition::Hue {
                degrees: 240.0,
                tone: Tone::Normal
            })
        );
        assert_eq!(
            colours.get("gamma"),
            Some(&Definition::Hue {
                degrees: 210.0,
                tone: Tone::Light
            })
        );
        assert_eq!(colours.get("pnl"), Some(&Definition::Token(Token::Bullish)));
        assert_eq!(
            colours.get("wrap"),
            Some(&Definition::Hue {
                degrees: 0.0,
                tone: Tone::Normal
            }),
            "360 is 0"
        );
        assert!(colours.get("both").is_none() && colours.get("neither").is_none());
        let errors: Vec<&str> = diags
            .iter()
            .filter(|d| d.severity == crate::config::Severity::Error)
            .filter_map(|d| d.path.as_deref())
            .collect();
        assert_eq!(errors, vec!["colours.both", "colours.neither"]);
        assert_eq!(
            colours.names().collect::<Vec<_>>(),
            vec!["delta", "gamma", "pnl", "wrap"],
            "sorted"
        );
    }

    #[test]
    fn reserved_names_and_bad_values_are_refused_with_paths() {
        let (colours, diags) = NamedColours::from_doc(&doc(
            "[sign]\nhue = 1\n[big]\nhue = 400\n[tok]\ntoken = \"nope\"\n[tone]\ntoken = \"danger\"\ntone = \"light\"\n",
        ));
        assert!(
            colours.get("sign").is_none()
                && colours.get("big").is_none()
                && colours.get("tok").is_none()
        );
        assert_eq!(
            colours.get("tone"),
            Some(&Definition::Token(Token::Danger)),
            "tone beside token is ignored with a warning"
        );
        let paths: Vec<&str> = diags.iter().filter_map(|d| d.path.as_deref()).collect();
        assert!(
            paths.contains(&"colours.sign")
                && paths.contains(&"colours.big.hue")
                && paths.contains(&"colours.tok.token")
                && paths.contains(&"colours.tone.tone"),
            "{paths:?}"
        );
    }

    #[test]
    fn an_anchor_hue_is_the_themes_own_colour_exactly() {
        let a = anchors();
        assert_eq!(interpolate_hue(240.0, Tone::Normal, &a), a.normal[4]);
        assert_eq!(interpolate_hue(0.0, Tone::Light, &a), a.light[0]);
        assert_eq!(interpolate_hue(360.0, Tone::Normal, &a), a.normal[0]);
    }

    #[test]
    fn a_hue_between_anchors_interpolates_along_the_shorter_arc() {
        let a = anchors();
        let mid = interpolate_hue(30.0, Tone::Normal, &a);
        let lch = oklab::lab_to_lch(oklab::srgb_to_oklab(mid));
        let red = oklab::lab_to_lch(oklab::srgb_to_oklab(a.normal[0]));
        let yellow = oklab::lab_to_lch(oklab::srgb_to_oklab(a.normal[1]));
        assert!(
            lch.h > red.h.min(yellow.h) && lch.h < red.h.max(yellow.h),
            "between red and yellow: {lch:?}"
        );
        assert!(
            lch.c > 0.6 * red.c.min(yellow.c),
            "chroma kept, not greyed: {lch:?}"
        );
        // 350 → 10 passes through red, not the long way round through cyan.
        let near_red = interpolate_hue(350.0, Tone::Normal, &a);
        let magenta = oklab::lab_to_lch(oklab::srgb_to_oklab(a.normal[5]));
        let got = oklab::lab_to_lch(oklab::srgb_to_oklab(near_red));
        let arc = |x: f32, y: f32| {
            ((x - y + std::f32::consts::PI).rem_euclid(std::f32::consts::TAU)
                - std::f32::consts::PI)
                .abs()
        };
        assert!(
            arc(got.h, red.h) < arc(magenta.h, red.h),
            "closer to red than magenta is: {got:?}"
        );
        // The comparative check above is too weak on its own to pin the
        // wrap correction down (the harness's "hue takes the shorter
        // arc" mutation survived it: dropping the wrap still lands
        // numerically closer to red than to magenta on this fixture,
        // since these six anchors are not evenly spaced 60° apart in
        // real OKLab hue). An absolute bound is airtight: t = 0.833 of
        // the way from magenta to red along the ~58° short arc must
        // land within a few degrees of red, never a wrong-way overshoot
        // past it.
        assert!(
            arc(got.h, red.h) < 0.3,
            "close to red, not overshooting past it the wrong way: {got:?}"
        );
    }

    #[test]
    fn resolve_uses_the_token_field_and_the_tone_anchors() {
        let (a, t) = (anchors(), tokens());
        assert_eq!(
            resolve(&Definition::Token(Token::Bearish), &a, &t),
            t.bearish
        );
        assert_eq!(
            resolve(&Definition::Token(Token::Chart(3)), &a, &t),
            t.chart[2]
        );
        assert_eq!(
            resolve(
                &Definition::Hue {
                    degrees: 120.0,
                    tone: Tone::Light
                },
                &a,
                &t
            ),
            a.light[2]
        );
    }

    #[test]
    fn contrast_ratio_is_wcag() {
        let ratio = contrast_ratio(
            Rgb {
                r: 1.0,
                g: 1.0,
                b: 1.0,
            },
            Rgb {
                r: 0.0,
                g: 0.0,
                b: 0.0,
            },
        );
        assert!((ratio - 21.0).abs() < 0.01, "{ratio}");
    }

    #[test]
    fn readable_on_pulls_a_faint_colour_darker_until_it_clears() {
        let background = Rgb {
            r: 0.95,
            g: 0.95,
            b: 0.95,
        };
        let toward = Rgb {
            r: 0.05,
            g: 0.05,
            b: 0.05,
        };
        let faint = Rgb {
            r: 0.92,
            g: 0.88,
            b: 0.7,
        };
        assert!(
            contrast_ratio(faint, background) < READABLE_RATIO,
            "fixture check: starts unreadable"
        );

        let result = readable_on(faint, background, toward);

        assert!(
            contrast_ratio(result, background) >= READABLE_RATIO,
            "{result:?}"
        );
        let orig = lab_to_lch(srgb_to_oklab(faint));
        let got = lab_to_lch(srgb_to_oklab(result));
        assert!(
            (got.h - orig.h).abs() < 0.02,
            "hue kept: {got:?} vs {orig:?}"
        );
        assert!(
            got.c <= orig.c + 1e-4,
            "chroma not increased: {got:?} vs {orig:?}"
        );
        assert!(got.l < orig.l, "pulled darker: {got:?} vs {orig:?}");
    }

    #[test]
    fn readable_on_returns_an_already_clearing_colour_unchanged() {
        let background = Rgb {
            r: 0.95,
            g: 0.95,
            b: 0.95,
        };
        let toward = Rgb {
            r: 0.05,
            g: 0.05,
            b: 0.05,
        };
        let dark_blue = Rgb {
            r: 0.1,
            g: 0.1,
            b: 0.6,
        };
        assert!(
            contrast_ratio(dark_blue, background) >= READABLE_RATIO,
            "fixture check: already clears"
        );

        assert_eq!(readable_on(dark_blue, background, toward), dark_blue);
    }

    #[test]
    fn resolve_of_an_anchor_hue_equals_the_anchor_when_readable_and_only_lightness_moves_when_not()
    {
        let background = Rgb {
            r: 0.95,
            g: 0.95,
            b: 0.95,
        };
        let readable_anchor = Rgb {
            r: 0.1,
            g: 0.1,
            b: 0.6,
        };
        let faint_anchor = Rgb {
            r: 0.92,
            g: 0.88,
            b: 0.7,
        };
        let mut normal = [readable_anchor; 6];
        normal[1] = faint_anchor; // ANCHOR_DEGREES[1] == 60.0
        let a = Anchors {
            normal,
            light: normal,
        };
        let mut t = tokens();
        t.background = background;
        t.foreground = Rgb {
            r: 0.05,
            g: 0.05,
            b: 0.05,
        };

        // Already readable: resolve is the anchor itself, exactly.
        assert_eq!(
            resolve(
                &Definition::Hue {
                    degrees: 0.0,
                    tone: Tone::Normal
                },
                &a,
                &t
            ),
            readable_anchor
        );

        // Not readable: resolve clears the floor and differs only in
        // lightness — hue and chroma (within the gamut clip's own
        // tolerance) are kept, unlike the raw anchor.
        let floored = resolve(
            &Definition::Hue {
                degrees: 60.0,
                tone: Tone::Normal,
            },
            &a,
            &t,
        );
        assert_ne!(floored, faint_anchor);
        assert!(contrast_ratio(floored, background) >= READABLE_RATIO);
        let orig = lab_to_lch(srgb_to_oklab(faint_anchor));
        let got = lab_to_lch(srgb_to_oklab(floored));
        assert!(
            (got.h - orig.h).abs() < 0.02,
            "hue kept: {got:?} vs {orig:?}"
        );
        assert!(
            got.c <= orig.c + 1e-4,
            "chroma not increased: {got:?} vs {orig:?}"
        );
        assert!(got.l < orig.l, "lightness moved: {got:?} vs {orig:?}");
    }

    #[test]
    fn token_names_round_trip() {
        for token in Token::ALL {
            assert_eq!(Token::parse(token.name()), Some(token), "{token:?}");
        }
        assert_eq!(Token::parse("chart.6"), None);
        assert_eq!(
            Definition::Hue {
                degrees: 210.0,
                tone: Tone::Light
            }
            .summary(),
            "hue 210 · light"
        );
        assert_eq!(
            Definition::Token(Token::Bullish).summary(),
            "token chart.bullish"
        );
    }
}
