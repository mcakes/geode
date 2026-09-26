//! Named colors are theme-relative hues or semantic tokens read from
//! configuration. Callers provide RGB anchors and tokens; this module has no
//! GPUI dependency. Interpolation, optional sign tinting, and contrast
//! adjustment are shared by cells, swatches, and charts.

pub mod oklab;

use crate::config::{Diagnostic, MergedDoc, Severity, check_object_name};
pub use crate::format::Sign;
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

/// Theme colors a definition can reference directly.
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

/// How a color's base is defined: a point on the canonical wheel the
/// theme's anchors transform, or one of the theme's own tokens.
#[derive(Debug, Clone, PartialEq)]
pub enum Base {
    Hue { degrees: f32, tone: Tone },
    Token(Token),
}

/// A named color: its [`Base`] plus whether a signed number painted in
/// it shifts its hue by sign (`tint_sign`, see [`tint`]). The tint is
/// orthogonal to how the base is defined — a hue and a token both tint
/// — which is why it is a field beside the base rather than a variant.
#[derive(Debug, Clone, PartialEq)]
pub struct Definition {
    pub base: Base,
    pub tint_sign: bool,
}

impl Definition {
    pub fn hue(degrees: f32, tone: Tone) -> Definition {
        Definition {
            base: Base::Hue { degrees, tone },
            tint_sign: false,
        }
    }
    pub fn token(token: Token) -> Definition {
        Definition {
            base: Base::Token(token),
            tint_sign: false,
        }
    }
    /// The same color with `tint_sign` on.
    pub fn tinted(mut self) -> Definition {
        self.tint_sign = true;
        self
    }

    /// The browse summary: `hue 240`, `hue 210 · light`, `token chart.bullish`,
    /// each with ` · ±sign` appended when the color tints by sign.
    pub fn summary(&self) -> String {
        let mut out = match &self.base {
            Base::Hue {
                degrees,
                tone: Tone::Normal,
            } => format!("hue {}", *degrees as i64),
            Base::Hue {
                degrees,
                tone: Tone::Light,
            } => format!("hue {} · light", *degrees as i64),
            Base::Token(t) => format!("token {}", t.name()),
        };
        if self.tint_sign {
            out.push_str(" · ±sign");
        }
        out
    }
}

/// A column's `color` key already spells these two.
pub const RESERVED_NAMES: [&str; 2] = ["none", "sign"];

/// A name starting with this is reserved too: `#rrggbb` is an absolute
/// color wherever a color name is also accepted (a timeseries slot's
/// `:color` and its session entry), so such a name would be ambiguous.
pub const RESERVED_PREFIX: char = '#';

#[derive(Debug, Clone, Default, PartialEq)]
pub struct NamedColours {
    by_name: BTreeMap<String, Definition>,
}

/// Report mutually exclusive `hue` and `token` fields. The caller skips
/// the rejected definition after recording this diagnostic.
fn refuse_both(diags: &mut Vec<Diagnostic>, at: &dyn Fn(&str) -> String, name: &str) {
    diags.push(Diagnostic {
        severity: Severity::Error,
        layer: None,
        file: None,
        message: format!(
            "color '{name}': both 'hue' and 'token' — a color is one or the other; dropped"
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
                    format!("{}.{name}", crate::config::COLORS_DOC)
                } else {
                    format!("{}.{name}.{suffix}", crate::config::COLORS_DOC)
                }
            };
            if RESERVED_NAMES.contains(&name.as_str())
                || name.starts_with(RESERVED_PREFIX)
                || check_object_name(name).is_err()
            {
                diags.push(diag(
                    Severity::Error,
                    at(""),
                    format!(
                        "color '{name}': the name is reserved or not a valid object name — dropped"
                    ),
                ));
                continue;
            }
            let Some(table) = value.as_table() else {
                diags.push(diag(
                    Severity::Error,
                    at(""),
                    format!("color '{name}': not a table — dropped"),
                ));
                continue;
            };
            let hue = table.get("hue");
            let token = table.get("token");
            // Read ahead of the hue/token split: the key is valid beside
            // either, so neither arm owns it.
            let tint_sign = match table.get("tint_sign") {
                None => false,
                Some(v) => match v.as_bool() {
                    Some(b) => b,
                    None => {
                        diags.push(diag(
                            Severity::Warning,
                            at("tint_sign"),
                            format!(
                                "color '{name}': 'tint_sign' must be true or false (got {v}); using false"
                            ),
                        ));
                        false
                    }
                },
            };
            let base = match (hue, token) {
                (Some(_), Some(_)) => {
                    refuse_both(&mut diags, &at, name);
                    continue;
                }
                (None, None) => {
                    diags.push(diag(
                        Severity::Error,
                        at(""),
                        format!("color '{name}': neither 'hue' nor 'token'; dropped"),
                    ));
                    continue;
                }
                (Some(h), None) => {
                    let Some(degrees) = h.as_float().or_else(|| h.as_integer().map(|i| i as f64))
                    else {
                        diags.push(diag(
                            Severity::Error,
                            at("hue"),
                            format!("color '{name}': 'hue' must be a number (got {h}); dropped"),
                        ));
                        continue;
                    };
                    if !(0.0..=360.0).contains(&degrees) {
                        diags.push(diag(
                            Severity::Error,
                            at("hue"),
                            format!(
                                "color '{name}': 'hue' must be 0..360 (got {degrees}); dropped"
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
                                    "color '{name}': 'tone' must be \"normal\" or \"light\" (got {other:?}); using normal"
                                ),
                            ));
                            Tone::Normal
                        }
                    };
                    Base::Hue {
                        degrees: (degrees % 360.0) as f32,
                        tone,
                    }
                }
                (None, Some(t)) => {
                    if table.get("tone").is_some() {
                        diags.push(diag(
                            Severity::Warning,
                            at("tone"),
                            format!("color '{name}': 'tone' has no effect beside 'token'; ignored"),
                        ));
                    }
                    match t.as_str().and_then(Token::parse) {
                        Some(token) => Base::Token(token),
                        None => {
                            diags.push(diag(
                                Severity::Error,
                                at("token"),
                                format!("color '{name}': unknown token {t}; dropped"),
                            ));
                            continue;
                        }
                    }
                }
            };
            out.by_name
                .insert(name.clone(), Definition { base, tint_sign });
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

/// Canonical wheel: red, yellow, green, cyan, blue, magenta.
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
    /// The theme's own background — not a [`Token`] a `colors.toml`
    /// definition can name (there is no `token = "background"`); it is
    /// the surface [`readable_on`] measures every generated `Base::Hue`
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

/// Interpolate between bracketing anchors in OKLCH: lightness and chroma
/// linearly, hue along the shorter arc. Exact anchor angles return the
/// anchor unchanged. This function does not check background contrast;
/// `resolve` applies `readable_on` afterward.
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

/// Target contrast ratio for generated colors against the theme background.
/// `readable_on` may fall short if the supplied theme offers no reachable
/// lightness with sufficient contrast.
pub const READABLE_RATIO: f32 = 3.0;

/// Move OKLCH lightness toward `toward` to seek `READABLE_RATIO` against
/// `background`, retaining hue and clipping chroma to the display gamut.
/// Already-readable colors are unchanged; otherwise 16 bisection steps
/// choose the adjustment. If no point on this lightness path clears the
/// ratio, the endpoint is returned without a contrast guarantee.
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

/// Resolve a hue or token against a theme. Hues pass through `readable_on`;
/// exact anchors stay unchanged when their contrast is sufficient. Tokens
/// are returned unchanged as the theme's semantic colors. `tint_sign` is
/// applied only by `resolve_signed`.
pub fn resolve(def: &Definition, anchors: &Anchors, tokens: &Tokens) -> Rgb {
    match &def.base {
        Base::Hue { degrees, tone } => readable_on(
            interpolate_hue(*degrees, *tone, anchors),
            tokens.background,
            tokens.foreground,
        ),
        Base::Token(token) => tokens.get(*token),
    }
}

/// Resolve a signed cell color. Without `tint_sign`, the sign is ignored.
/// With tinting, all three sign variants, including zero and token bases,
/// pass through `readable_on` so zero/header cells do not bypass the contrast
/// adjustment. Achievable contrast still depends on the theme's colors.
/// Achromatic bases have no visible hue shift.
pub fn resolve_signed(def: &Definition, sign: Sign, anchors: &Anchors, tokens: &Tokens) -> Rgb {
    let base = resolve(def, anchors, tokens);
    if !def.tint_sign {
        return base;
    }
    readable_on(tint(base, sign), tokens.background, tokens.foreground)
}

/// Maximum sign-tint rotation in OKLCH degrees. Positive and negative
/// variants move toward opposite poles while retaining the base identity.
pub const TINT_DEGREES: f32 = 40.0;
/// The warm pole of the OKLCH wheel (orange) a negative number moves
/// toward, and the cool pole (azure) a positive one moves toward — one
/// axis, 180° apart.
pub const WARM_POLE_DEGREES: f32 = 50.0;
pub const COOL_POLE_DEGREES: f32 = 230.0;

/// Rotate `rgb`'s OKLCH hue [`TINT_DEGREES`] toward the cool pole for a
/// positive sign or the warm pole for a negative one, along the shorter
/// arc, stopping at the pole; lightness and chroma are kept (re-clipped
/// to gamut). `Sign::Zero` is `rgb` itself. Sitting exactly on the pole
/// it moves away from, the shorter arc is a tie: the wrap arithmetic
/// picks a side (which one depends on the last bit of the round-tripped
/// hue, so the direction is not a contract), and both variants still
/// differ from the base by a full step, which is.
pub fn tint(rgb: Rgb, sign: Sign) -> Rgb {
    let pole = match sign {
        Sign::Zero => return rgb,
        Sign::Positive => COOL_POLE_DEGREES,
        Sign::Negative => WARM_POLE_DEGREES,
    };
    let lch = lab_to_lch(srgb_to_oklab(rgb));
    let to_pole = (pole.to_radians() - lch.h + PI).rem_euclid(TAU) - PI; // the shorter arc
    let step = TINT_DEGREES.to_radians().min(to_pole.abs());
    let dh = if to_pole < 0.0 { -step } else { step };
    to_srgb_in_gamut(Lch {
        l: lch.l,
        c: lch.c,
        h: (lch.h + dh).rem_euclid(TAU),
    })
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
        merge_docs("colors", &[LayerDoc::builtin("colors", text).unwrap()])
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
            Some(&Definition::hue(240.0, Tone::Normal))
        );
        assert_eq!(
            colours.get("gamma"),
            Some(&Definition::hue(210.0, Tone::Light))
        );
        assert_eq!(colours.get("pnl"), Some(&Definition::token(Token::Bullish)));
        assert_eq!(
            colours.get("wrap"),
            Some(&Definition::hue(0.0, Tone::Normal)),
            "360 is 0"
        );
        assert!(colours.get("both").is_none() && colours.get("neither").is_none());
        let errors: Vec<&str> = diags
            .iter()
            .filter(|d| d.severity == crate::config::Severity::Error)
            .filter_map(|d| d.path.as_deref())
            .collect();
        assert_eq!(errors, vec!["colors.both", "colors.neither"]);
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
            Some(&Definition::token(Token::Danger)),
            "tone beside token is ignored with a warning"
        );
        let paths: Vec<&str> = diags.iter().filter_map(|d| d.path.as_deref()).collect();
        assert!(
            paths.contains(&"colors.sign")
                && paths.contains(&"colors.big.hue")
                && paths.contains(&"colors.tok.token")
                && paths.contains(&"colors.tone.tone"),
            "{paths:?}"
        );
    }

    /// A `#` name is refused: `#rrggbb` is how a series' absolute color
    /// is spelled wherever a color name is also accepted, so a name
    /// starting with one could never be told from it.
    #[test]
    fn a_name_starting_with_a_hash_is_reserved() {
        let (colours, diags) =
            NamedColours::from_doc(&doc("[\"#ff8800\"]\nhue = 30\n[ok]\nhue = 30\n"));
        assert!(colours.get("#ff8800").is_none());
        assert!(colours.get("ok").is_some());
        let paths: Vec<&str> = diags.iter().filter_map(|d| d.path.as_deref()).collect();
        assert_eq!(paths, vec!["colors.#ff8800"]);
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
        // The anchor hues are not evenly spaced in OKLab, so proximity alone
        // cannot prove the direction of interpolation. Bound the result near red
        // to exclude a long-arc overshoot as well as a distant midpoint.
        assert!(
            arc(got.h, red.h) < 0.3,
            "close to red, not overshooting past it the wrong way: {got:?}"
        );
    }

    #[test]
    fn resolve_uses_the_token_field_and_the_tone_anchors() {
        let (a, t) = (anchors(), tokens());
        assert_eq!(
            resolve(&Definition::token(Token::Bearish), &a, &t),
            t.bearish
        );
        assert_eq!(
            resolve(&Definition::token(Token::Chart(3)), &a, &t),
            t.chart[2]
        );
        assert_eq!(
            resolve(&Definition::hue(120.0, Tone::Light), &a, &t),
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
            resolve(&Definition::hue(0.0, Tone::Normal), &a, &t),
            readable_anchor
        );

        // Not readable: resolve clears the floor and differs only in
        // lightness — hue and chroma (within the gamut clip's own
        // tolerance) are kept, unlike the raw anchor.
        let floored = resolve(&Definition::hue(60.0, Tone::Normal), &a, &t);
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
            Definition::hue(210.0, Tone::Light).summary(),
            "hue 210 · light"
        );
        assert_eq!(
            Definition::token(Token::Bullish).summary(),
            "token chart.bullish"
        );
        assert_eq!(
            Definition::hue(210.0, Tone::Light).tinted().summary(),
            "hue 210 · light · ±sign"
        );
        assert_eq!(
            Definition::token(Token::Chart(3)).tinted().summary(),
            "token chart.3 · ±sign"
        );
    }

    #[test]
    fn reads_tint_sign_beside_a_hue_or_a_token_and_warns_on_a_non_bool() {
        let (colours, diags) = NamedColours::from_doc(&doc(
            "[delta]\nhue = 240\ntint_sign = true\n[pnl]\ntoken = \"chart.3\"\ntint_sign = true\n\
             [off]\nhue = 10\ntint_sign = false\n[bad]\nhue = 20\ntint_sign = \"yes\"\n",
        ));
        assert_eq!(
            colours.get("delta"),
            Some(&Definition::hue(240.0, Tone::Normal).tinted())
        );
        assert_eq!(
            colours.get("pnl"),
            Some(&Definition::token(Token::Chart(3)).tinted()),
            "a token tints too"
        );
        assert_eq!(
            colours.get("off"),
            Some(&Definition::hue(10.0, Tone::Normal))
        );
        assert_eq!(
            colours.get("bad"),
            Some(&Definition::hue(20.0, Tone::Normal)),
            "a non-bool is a warning and off, never a dropped colour"
        );
        let warning = diags
            .iter()
            .find(|d| d.path.as_deref() == Some("colors.bad.tint_sign"))
            .expect("a diagnostic at the key");
        assert_eq!(warning.severity, crate::config::Severity::Warning);
    }

    /// An in-gamut color at OKLCH hue `degrees`, moderate chroma.
    fn at_hue(degrees: f32) -> Rgb {
        oklab::oklab_to_srgb(oklab::lch_to_lab(Lch {
            l: 0.6,
            c: 0.1,
            h: degrees.to_radians(),
        }))
    }
    fn hue_of(rgb: Rgb) -> f32 {
        lab_to_lch(srgb_to_oklab(rgb))
            .h
            .to_degrees()
            .rem_euclid(360.0)
    }
    /// Unsigned arc between two hues in degrees, wrap-safe.
    fn arc(a: f32, b: f32) -> f32 {
        ((a - b + 180.0).rem_euclid(360.0) - 180.0).abs()
    }

    /// The maximum tint step preserves a visible separation between signs
    /// while keeping both variants near the base hue.
    #[test]
    fn the_tint_step_is_forty_degrees() {
        assert_eq!(TINT_DEGREES, 40.0);
    }

    #[test]
    fn tint_rotates_positive_toward_the_cool_pole_and_negative_toward_the_warm_pole() {
        // Green (142°): the cool pole (230°) is anticlockwise of it, the
        // warm pole (50°) clockwise — so positive raises the hue and
        // negative lowers it, each by exactly TINT_DEGREES.
        let green = at_hue(142.0);
        let positive = tint(green, Sign::Positive);
        let negative = tint(green, Sign::Negative);
        assert!(
            arc(hue_of(positive), 142.0 + TINT_DEGREES) < 0.5,
            "cooler: {}",
            hue_of(positive)
        );
        assert!(
            arc(hue_of(negative), 142.0 - TINT_DEGREES) < 0.5,
            "warmer: {}",
            hue_of(negative)
        );
        assert_eq!(tint(green, Sign::Zero), green, "zero is the base itself");
        // Magenta-red (350°): the cool pole is now reached the OTHER way
        // round the wheel (350 → 310 is the shorter arc to 230), and
        // warm is upward through 0 (350 → 30). Both poles are more than
        // a step away, so neither variant is clamped.
        let magenta = at_hue(350.0);
        assert!(
            arc(hue_of(tint(magenta, Sign::Positive)), 350.0 - TINT_DEGREES) < 0.5,
            "cooler from magenta-red goes down toward blue: {}",
            hue_of(tint(magenta, Sign::Positive))
        );
        assert!(
            arc(hue_of(tint(magenta, Sign::Negative)), 350.0 + TINT_DEGREES) < 0.5,
            "warmer from magenta-red goes up through red: {}",
            hue_of(tint(magenta, Sign::Negative))
        );
        // Lightness and chroma are kept, not traded for the rotation.
        let (base, got) = (
            lab_to_lch(srgb_to_oklab(green)),
            lab_to_lch(srgb_to_oklab(positive)),
        );
        assert!((base.l - got.l).abs() < 0.01, "{base:?} vs {got:?}");
        assert!((base.c - got.c).abs() < 0.01, "{base:?} vs {got:?}");
    }

    #[test]
    fn tint_stops_at_the_pole_it_is_moving_toward() {
        // 220° is 10° short of the cool pole: positive lands ON the pole
        // (not 10° past it), negative moves the full step away.
        let azure = at_hue(220.0);
        assert!(
            arc(hue_of(tint(azure, Sign::Positive)), COOL_POLE_DEGREES) < 0.5,
            "{}",
            hue_of(tint(azure, Sign::Positive))
        );
        assert!(
            arc(hue_of(tint(azure, Sign::Negative)), 220.0 - TINT_DEGREES) < 0.5,
            "{}",
            hue_of(tint(azure, Sign::Negative))
        );
        // Sitting exactly on the warm pole, negative is the base itself
        // and positive moves the full step away.
        let orange = at_hue(WARM_POLE_DEGREES);
        assert!(arc(hue_of(tint(orange, Sign::Negative)), WARM_POLE_DEGREES) < 0.5);
        assert!(arc(hue_of(tint(orange, Sign::Positive)), WARM_POLE_DEGREES) > TINT_DEGREES - 0.5);
    }

    #[test]
    fn resolve_signed_is_the_base_unless_the_definition_tints_and_the_sign_is_nonzero() {
        let (a, mut t) = (anchors(), tokens());
        t.chart[2] = at_hue(142.0);
        let plain = Definition::token(Token::Chart(3));
        for sign in [Sign::Negative, Sign::Zero, Sign::Positive] {
            assert_eq!(
                resolve_signed(&plain, sign, &a, &t),
                resolve(&plain, &a, &t),
                "{sign:?}: an untinted definition ignores the sign"
            );
        }
        let tinted = plain.clone().tinted();
        assert_eq!(
            resolve_signed(&tinted, Sign::Zero, &a, &t),
            resolve(&tinted, &a, &t),
            "zero is the base"
        );
        let positive = resolve_signed(&tinted, Sign::Positive, &a, &t);
        let negative = resolve_signed(&tinted, Sign::Negative, &a, &t);
        assert_eq!(positive, tint(t.chart[2], Sign::Positive));
        assert_eq!(negative, tint(t.chart[2], Sign::Negative));
        assert_ne!(positive, negative);
    }

    /// Untinted semantic tokens remain unchanged. Tinted tokens apply contrast
    /// adjustment to the entire sign triad, including the zero/header color.
    #[test]
    fn a_tinted_token_is_floored_as_a_whole_triad_but_an_untinted_one_is_not() {
        let (a, mut t) = (anchors(), tokens());
        t.background = grey(0.95);
        t.foreground = grey(0.05);
        // Faint on the light background: unreadable as the author left it.
        let faint = Rgb {
            r: 0.92,
            g: 0.88,
            b: 0.7,
        };
        assert!(contrast_ratio(faint, t.background) < READABLE_RATIO);
        t.chart[0] = faint;
        let plain = Definition::token(Token::Chart(1));
        for sign in [Sign::Negative, Sign::Zero, Sign::Positive] {
            assert_eq!(
                resolve_signed(&plain, sign, &a, &t),
                faint,
                "{sign:?}: an untinted token is the author's own colour"
            );
        }
        let tinted = plain.tinted();
        for sign in [Sign::Negative, Sign::Zero, Sign::Positive] {
            let got = resolve_signed(&tinted, sign, &a, &t);
            assert!(
                contrast_ratio(got, t.background) >= READABLE_RATIO,
                "{sign:?}: {got:?}"
            );
        }
        let (zero, orig) = (
            lab_to_lch(srgb_to_oklab(resolve_signed(&tinted, Sign::Zero, &a, &t))),
            lab_to_lch(srgb_to_oklab(faint)),
        );
        assert!(
            (zero.h - orig.h).abs() < 0.02 && zero.l < orig.l,
            "the zero variant is the base with only its lightness moved: {zero:?} vs {orig:?}"
        );
    }
}
