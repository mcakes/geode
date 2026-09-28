//! Cached named colours for the theme's resolved inputs, shared by every tile that paints a named colour.
//!
//! Each defined name resolves its base and sign variants once per cache fill.
//! Hue definitions require interpolation and gamut clipping; cell painting
//! reuses the result. A changed [`Anchors`]/[`Tokens`] pair clears the map on
//! lookup. Definition changes require an explicit [`ColourCache::invalidate`].
//!
//! The delegate memoizes theme-to-input conversion separately and supplies
//! these plain values, so this cache has no dependency on a GPUI theme object.

use geode_core::colour::{Anchors, NamedColours, Sign, Tokens, resolve_signed};
use geode_shell::shell::colours::to_hsla;
use gpui::Hsla;
use std::collections::HashMap;

/// One named colour resolved for every sign a cell can carry: the base,
/// and the two `tint_sign` variants (both equal to the base when the
/// definition does not tint). All three are resolved on the one cache
/// miss, so the per-cell pick ([`Resolved::for_sign`]) is a field read.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Resolved {
    pub base: Hsla,
    pub positive: Hsla,
    pub negative: Hsla,
}

impl Resolved {
    /// An untinted colour: the base three times over.
    pub fn plain(base: Hsla) -> Resolved {
        Resolved {
            base,
            positive: base,
            negative: base,
        }
    }

    /// The variant for a cell of `sign`; `None` (a header, a cell with
    /// no number) and `Zero` are the base.
    pub fn for_sign(self, sign: Option<Sign>) -> Hsla {
        match sign {
            Some(Sign::Positive) => self.positive,
            Some(Sign::Negative) => self.negative,
            Some(Sign::Zero) | None => self.base,
        }
    }
}

#[derive(Debug, Default)]
pub struct ColourCache {
    /// The theme inputs every entry in `by_name` was resolved under.
    /// `Anchors`/`Tokens` are plain `Copy` structs of floats — comparable,
    /// not hashable — so this is one compare per lookup, not a hash.
    key: Option<(Anchors, Tokens)>,
    by_name: HashMap<String, Resolved>,
    misses: u64,
}

impl ColourCache {
    pub fn new() -> Self {
        ColourCache::default()
    }

    /// Resolve all three sign variants for `name`, reusing the result while
    /// the theme inputs and definitions remain unchanged. Undefined names
    /// return `None`; the delegate uses the theme foreground as fallback.
    /// Call [`Self::invalidate`] whenever the definitions change.
    ///
    /// A defined-name miss allocates one key string. Hits borrow the name
    /// for lookup, and undefined names allocate nothing and are not cached.
    pub fn get(
        &mut self,
        colours: &NamedColours,
        name: &str,
        anchors: &Anchors,
        tokens: &Tokens,
    ) -> Option<Resolved> {
        let key = (*anchors, *tokens);
        if self.key.as_ref() != Some(&key) {
            self.key = Some(key);
            self.by_name.clear();
            self.misses += 1;
        }
        if let Some(hit) = self.by_name.get(name) {
            return Some(*hit);
        }
        let def = colours.get(name)?;
        let resolved = Resolved {
            base: to_hsla(resolve_signed(def, Sign::Zero, anchors, tokens)),
            positive: to_hsla(resolve_signed(def, Sign::Positive, anchors, tokens)),
            negative: to_hsla(resolve_signed(def, Sign::Negative, anchors, tokens)),
        };
        self.by_name.insert(name.to_string(), resolved);
        Some(resolved)
    }

    /// Everything resolved so far is against the old definitions: drop
    /// it. Called by the delegate when the tile hands it a different
    /// `NamedColours` (a `colours.toml` reload), since the key above
    /// tracks the *theme*, not the doc — a redefined `delta` under an
    /// unchanged theme would otherwise keep painting its old colour for
    /// the life of the tile.
    pub fn invalidate(&mut self) {
        self.key = None;
        self.by_name.clear();
    }

    /// Count theme-key changes observed by `get`, including its first call
    /// and the first call after `invalidate`. Undefined-name lookups also
    /// count when they change the key; individual name resolutions do not.
    pub fn misses(&self) -> u64 {
        self.misses
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use geode_core::colour::{Definition, Rgb, Sign, Tone, resolve_signed};

    /// Six anchors, all grey but `blue` (index 4 of
    /// `ANCHOR_DEGREES`, the 240° one this test's definition names).
    fn anchors(blue: Rgb) -> Anchors {
        let grey = Rgb {
            r: 0.5,
            g: 0.5,
            b: 0.5,
        };
        let mut normal = [grey; 6];
        normal[4] = blue;
        Anchors {
            normal,
            light: [grey; 6],
        }
    }

    fn tokens() -> Tokens {
        let grey = Rgb {
            r: 0.5,
            g: 0.5,
            b: 0.5,
        };
        Tokens {
            foreground: grey,
            muted: grey,
            primary: grey,
            accent: grey,
            danger: grey,
            warning: grey,
            success: grey,
            info: grey,
            chart: [grey; 5],
            bullish: grey,
            bearish: grey,
            background: Rgb {
                r: 0.0,
                g: 0.0,
                b: 0.0,
            },
        }
    }

    #[test]
    fn a_steady_theme_costs_no_recompute_and_a_changed_anchor_empties_the_cache() {
        let mut colours = NamedColours::default();
        colours.insert("delta".into(), Definition::hue(240.0, Tone::Normal));
        let (tokens, a) = (
            tokens(),
            anchors(Rgb {
                r: 0.125,
                g: 0.25,
                b: 0.75,
            }),
        );
        let mut cache = ColourCache::new();

        let first = cache.get(&colours, "delta", &a, &tokens).unwrap();
        assert_eq!(cache.get(&colours, "delta", &a, &tokens), Some(first));
        assert_eq!(
            cache.misses(),
            1,
            "an unchanged theme must not re-resolve anything"
        );

        let b = anchors(Rgb {
            r: 0.25,
            g: 0.75,
            b: 0.125,
        });
        let second = cache.get(&colours, "delta", &b, &tokens).unwrap();
        assert_ne!(
            first, second,
            "a changed anchor must re-resolve, not serve the old colour"
        );
        assert_eq!(cache.misses(), 2);

        assert_eq!(
            cache.get(&colours, "ghost", &b, &tokens),
            None,
            "a name the doc does not define resolves to nothing at all"
        );
    }

    /// The other invalidation: same theme, new definitions. Nothing in
    /// the key can see a `colours.toml` edit, so the delegate tells the
    /// cache — and until it does, the old colour is what paints.
    #[test]
    fn invalidate_drops_colours_resolved_from_the_old_definitions() {
        let mut colours = NamedColours::default();
        colours.insert("delta".into(), Definition::hue(240.0, Tone::Normal));
        let (tokens, a) = (
            tokens(),
            anchors(Rgb {
                r: 0.125,
                g: 0.25,
                b: 0.75,
            }),
        );
        let mut cache = ColourCache::new();
        let before = cache.get(&colours, "delta", &a, &tokens).unwrap();

        let mut redefined = NamedColours::default();
        redefined.insert(
            "delta".into(),
            Definition::token(geode_core::colour::Token::Foreground),
        );
        assert_eq!(
            cache.get(&redefined, "delta", &a, &tokens),
            Some(before),
            "fixture check: without invalidation the stale colour is exactly what a redefine would keep painting"
        );
        cache.invalidate();
        assert_eq!(
            cache.get(&redefined, "delta", &a, &tokens),
            Some(Resolved::plain(to_hsla(tokens.foreground))),
            "after invalidation the new definition is what resolves"
        );
    }

    /// One entry holds all three variants, resolved together on the
    /// miss: a tinted definition's positive and negative differ from its
    /// base and from each other, an untinted one's are the base three
    /// times over — so `for_sign` is a field read either way and a cell
    /// never resolves anything per frame.
    #[test]
    fn a_tinted_definition_resolves_three_variants_and_an_untinted_one_three_of_the_base() {
        let mut colours = NamedColours::default();
        colours.insert(
            "plain".into(),
            Definition::token(geode_core::colour::Token::Chart(1)),
        );
        colours.insert(
            "tinted".into(),
            Definition::token(geode_core::colour::Token::Chart(1)).tinted(),
        );
        let mut tokens = tokens();
        // A saturated green so the rotation is visible in the result.
        tokens.chart[0] = Rgb {
            r: 0.2,
            g: 0.7,
            b: 0.3,
        };
        let a = anchors(tokens.chart[0]);
        let mut cache = ColourCache::new();

        let plain = cache.get(&colours, "plain", &a, &tokens).unwrap();
        assert_eq!(plain, Resolved::plain(to_hsla(tokens.chart[0])));
        for sign in [
            None,
            Some(Sign::Negative),
            Some(Sign::Zero),
            Some(Sign::Positive),
        ] {
            assert_eq!(plain.for_sign(sign), plain.base, "{sign:?}");
        }

        let tinted = cache.get(&colours, "tinted", &a, &tokens).unwrap();
        assert_eq!(tinted.base, plain.base, "the base is the token itself");
        assert_eq!(
            tinted.positive,
            to_hsla(resolve_signed(
                colours.get("tinted").unwrap(),
                Sign::Positive,
                &a,
                &tokens
            ))
        );
        assert_eq!(
            tinted.negative,
            to_hsla(resolve_signed(
                colours.get("tinted").unwrap(),
                Sign::Negative,
                &a,
                &tokens
            ))
        );
        assert_ne!(tinted.positive, tinted.base);
        assert_ne!(tinted.negative, tinted.base);
        assert_ne!(tinted.positive, tinted.negative);
        assert_eq!(tinted.for_sign(Some(Sign::Positive)), tinted.positive);
        assert_eq!(tinted.for_sign(Some(Sign::Negative)), tinted.negative);
        assert_eq!(tinted.for_sign(Some(Sign::Zero)), tinted.base);
        assert_eq!(
            tinted.for_sign(None),
            tinted.base,
            "a cell with no sign (a header, a NULL) is the base"
        );
    }
}
