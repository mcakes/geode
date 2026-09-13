//! One resolve per named colour per theme (Part 2c spec §6.3). The pure
//! resolver ([`geode_core::colour::resolve`]) walks OKLCH for a `hue`
//! definition — interpolation plus a gamut clip — which is far too much
//! to redo for every painted cell, and a blotter repaints every one of
//! them on every frame. So a definition is resolved once per
//! `(anchors, tokens)` pair and reused until the theme actually changes.
//!
//! Deliberately pure: it takes [`Anchors`]/[`Tokens`], never a
//! `gpui_component::Theme`, so the only gpui type in sight is the `Hsla`
//! it hands back. The caller (`BlotterDelegate::cell_colour`) reads the
//! theme once per `render_td`/`render_th` through
//! `geode_shell::shell::colours` and passes the pair down — which is
//! also what makes the invalidation honest: the key IS the theme's own
//! resolved inputs, so a theme swap, a light/dark pick or a hot-reloaded
//! theme file all empty the map by construction, with nothing to
//! remember to call.

use geode_core::colour::{Anchors, NamedColours, Tokens, resolve};
use geode_shell::shell::colours::to_hsla;
use gpui::Hsla;
use std::collections::HashMap;

#[derive(Debug, Default)]
pub struct ColourCache {
    /// The theme inputs every entry in `by_name` was resolved under.
    /// `Anchors`/`Tokens` are plain `Copy` structs of floats — comparable,
    /// not hashable — so this is one compare per lookup, not a hash.
    key: Option<(Anchors, Tokens)>,
    by_name: HashMap<String, Hsla>,
    misses: u64,
}

impl ColourCache {
    pub fn new() -> Self {
        ColourCache::default()
    }

    /// The colour for `name` under `(anchors, tokens)`, resolved once
    /// and reused until they change. `None` when the doc does not define
    /// `name` at all — the caller paints such a cell in the theme's
    /// foreground (§6.3), so a name the trader deleted from
    /// `colours.toml` is never left showing a stale colour.
    ///
    /// A miss costs one `String` (the key); a hit costs a hash lookup by
    /// `&str` and nothing else — the name is never cloned on the hit
    /// path, which is the one this runs on per cell per frame. An
    /// *undefined* name allocates nothing at all, so a view naming a
    /// colour that no longer exists is not a per-frame allocation
    /// either.
    pub fn get(
        &mut self,
        colours: &NamedColours,
        name: &str,
        anchors: &Anchors,
        tokens: &Tokens,
    ) -> Option<Hsla> {
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
        let hsla = to_hsla(resolve(def, anchors, tokens));
        self.by_name.insert(name.to_string(), hsla);
        Some(hsla)
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

    /// Test hook: how many times the map has been emptied and refilled
    /// from scratch — one at the first resolve, one more per theme
    /// change. A steady theme must never bump it, which is the whole
    /// point of the cache.
    pub fn misses(&self) -> u64 {
        self.misses
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use geode_core::colour::{Definition, Rgb, Tone};

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
        colours.insert(
            "delta".into(),
            Definition::Hue {
                degrees: 240.0,
                tone: Tone::Normal,
            },
        );
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
        colours.insert(
            "delta".into(),
            Definition::Hue {
                degrees: 240.0,
                tone: Tone::Normal,
            },
        );
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
            Definition::Token(geode_core::colour::Token::Foreground),
        );
        assert_eq!(
            cache.get(&redefined, "delta", &a, &tokens),
            Some(before),
            "fixture check: without invalidation the stale colour is exactly what a redefine would keep painting"
        );
        cache.invalidate();
        assert_eq!(
            cache.get(&redefined, "delta", &a, &tokens),
            Some(to_hsla(tokens.foreground)),
            "after invalidation the new definition is what resolves"
        );
    }
}
