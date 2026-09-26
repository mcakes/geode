//! A slot's absolute colour ([`Colour::Custom`]): an opaque 8-bit sRGB
//! triple, spelled `#rrggbb` on the `:` line and in `session.toml`, and
//! the pure mapping from a colour-picker answer back onto the slot's
//! colour vocabulary.

use gpui::{Hsla, Rgba};

use super::model::Colour;

/// An opaque sRGB colour, one byte a channel. Bytes rather than the
/// `f32` channels `geode_core::colour::Rgb` carries, so [`Colour`] keeps
/// `Eq` and a picked colour round-trips through `#rrggbb` exactly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Rgb8(pub [u8; 3]);

impl Rgb8 {
    /// `#rrggbb`, six hex digits in either case, and nothing else: the
    /// short and alpha forms the picker's own field accepts are refused,
    /// because a series is always opaque and one spelling is one colour.
    pub fn parse_hex(text: &str) -> Option<Rgb8> {
        let digits = text.strip_prefix('#')?;
        if digits.len() != 6 || !digits.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        let channel = |i: usize| u8::from_str_radix(&digits[i..i + 2], 16).ok();
        Some(Rgb8([channel(0)?, channel(2)?, channel(4)?]))
    }

    /// `#rrggbb`, lowercase — the one spelling written back.
    pub fn hex(self) -> String {
        let [r, g, b] = self.0;
        format!("#{r:02x}{g:02x}{b:02x}")
    }

    /// Alpha is dropped and each channel rounded to the nearest byte.
    pub fn from_hsla(h: Hsla) -> Rgb8 {
        let rgba = Rgba::from(h);
        let byte = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
        Rgb8([byte(rgba.r), byte(rgba.g), byte(rgba.b)])
    }

    pub fn to_hsla(self) -> Hsla {
        let [r, g, b] = self.0;
        Rgba {
            r: r as f32 / 255.0,
            g: g as f32 / 255.0,
            b: b as f32 / 255.0,
            a: 1.0,
        }
        .into()
    }
}

/// Whether two colours are the same to within one 8-bit step on every
/// channel. The picker's hex field TRUNCATES each channel where
/// [`Rgb8::from_hsla`] rounds, so a colour read back through that field
/// can come back one step low; a tolerance of one absorbs that and
/// nothing a trader could tell apart.
pub fn within_a_step(a: Rgb8, b: Rgb8) -> bool {
    a.0.iter().zip(b.0).all(|(x, y)| x.abs_diff(y) <= 1)
}

fn distance(a: Rgb8, b: Rgb8) -> u32 {
    a.0.iter().zip(b.0).map(|(x, y)| x.abs_diff(y) as u32).sum()
}

/// What a colour the picker answered with means for the slot. A pick
/// within a step ([`within_a_step`]) of a featured entry is that entry's
/// own `Palette` or `Named` colour — which keeps following the theme —
/// the nearest such entry, the first on a tie. Anything else (a
/// palette-grid swatch, a slider, a typed hex) is an absolute
/// [`Colour::Custom`]. A grid swatch that happens to equal a featured
/// colour is therefore read as that featured colour.
pub fn colour_from_pick(h: Hsla, featured: &[(Hsla, Colour)]) -> Colour {
    let picked = Rgb8::from_hsla(h);
    let nearest = featured
        .iter()
        .map(|(f, colour)| (Rgb8::from_hsla(*f), colour))
        .filter(|(f, _)| within_a_step(*f, picked))
        .min_by_key(|(f, _)| distance(*f, picked));
    match nearest {
        Some((_, colour)) => colour.clone(),
        None => Colour::Custom(picked),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_parses_six_digits_in_either_case_and_writes_lowercase() {
        let c = Rgb8::parse_hex("#FF8800").unwrap();
        assert_eq!(c, Rgb8([0xff, 0x88, 0x00]));
        assert_eq!(Rgb8::parse_hex("#ff8800"), Some(c));
        assert_eq!(c.hex(), "#ff8800");
        assert_eq!(Rgb8::parse_hex(&c.hex()), Some(c));
    }

    #[test]
    fn malformed_hex_is_refused() {
        for bad in [
            "ff8800",
            "#ff880",
            "#ff88001",
            "#fff",
            "#ff880080",
            "#gg8800",
            "#+f8800",
            "#",
            "",
            "#ff 800",
        ] {
            assert_eq!(Rgb8::parse_hex(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn an_hsla_round_trips_through_bytes_and_drops_alpha() {
        let c = Rgb8([0x12, 0xab, 0xef]);
        assert_eq!(Rgb8::from_hsla(c.to_hsla()), c);
        let translucent = Hsla {
            a: 0.25,
            ..c.to_hsla()
        };
        assert_eq!(Rgb8::from_hsla(translucent), c, "alpha is dropped");
        assert_eq!(c.to_hsla().a, 1.0, "a series is opaque");
    }

    #[test]
    fn a_featured_pick_is_that_entrys_own_colour_and_anything_else_is_custom() {
        let p0 = gpui::hsla(0.1, 0.8, 0.5, 1.0);
        let spx = gpui::hsla(0.6, 0.7, 0.4, 1.0);
        let featured = vec![(p0, Colour::Palette(0)), (spx, Colour::Named("spx".into()))];
        assert_eq!(colour_from_pick(p0, &featured), Colour::Palette(0));
        assert_eq!(
            colour_from_pick(spx, &featured),
            Colour::Named("spx".into())
        );
        let other = gpui::hsla(0.3, 0.5, 0.5, 1.0);
        assert_eq!(
            colour_from_pick(other, &featured),
            Colour::Custom(Rgb8::from_hsla(other))
        );
        assert_eq!(
            colour_from_pick(p0, &[]),
            Colour::Custom(Rgb8::from_hsla(p0)),
            "nothing featured: every pick is absolute"
        );
    }

    /// The component's hex field truncates where this crate rounds, so a
    /// featured colour read back through it can land one step low on any
    /// channel; that is still the featured colour. Two steps is not.
    #[test]
    fn a_pick_one_step_off_a_featured_colour_is_that_colour() {
        let base = Rgb8([0x68, 0x98, 0xce]);
        let featured = vec![(base.to_hsla(), Colour::Palette(0))];
        let one_low = Rgb8([0x67, 0x97, 0xcd]).to_hsla();
        assert_eq!(colour_from_pick(one_low, &featured), Colour::Palette(0));
        let one_high = Rgb8([0x69, 0x98, 0xce]).to_hsla();
        assert_eq!(colour_from_pick(one_high, &featured), Colour::Palette(0));
        let two_low = Rgb8([0x66, 0x98, 0xce]);
        assert_eq!(
            colour_from_pick(two_low.to_hsla(), &featured),
            Colour::Custom(two_low)
        );
        assert!(within_a_step(base, Rgb8([0x67, 0x99, 0xce])));
        assert!(!within_a_step(base, Rgb8([0x68, 0x98, 0xd0])));
    }

    /// Two featured colours inside the tolerance: the nearer wins, and
    /// on a tie the earlier one.
    #[test]
    fn the_nearest_featured_colour_wins_then_the_first() {
        let a = Rgb8([10, 10, 10]);
        let b = Rgb8([11, 11, 11]);
        let featured = vec![
            (a.to_hsla(), Colour::Palette(0)),
            (b.to_hsla(), Colour::Palette(1)),
        ];
        assert_eq!(colour_from_pick(b.to_hsla(), &featured), Colour::Palette(1));
        assert_eq!(colour_from_pick(a.to_hsla(), &featured), Colour::Palette(0));
        let tied = vec![
            (b.to_hsla(), Colour::Palette(2)),
            (b.to_hsla(), Colour::Palette(3)),
        ];
        assert_eq!(colour_from_pick(b.to_hsla(), &tied), Colour::Palette(2));
    }
}
