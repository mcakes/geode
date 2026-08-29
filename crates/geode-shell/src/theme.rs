//! Theming (Task 5): gpui-component's bundled theme JSONs, selectable from
//! config, with a runtime light/dark mode toggle. Geode is a lens, not a
//! brand exercise (PHILOSOPHY.md): theming stays entirely inside
//! gpui-component's own theme-config format, applied through its own theme
//! global — nothing here invents a raw color.
//!
//! ## Checkout findings (gpui-component rev `0e2fb7a`, pinned in the
//! workspace root `Cargo.toml`)
//!
//! Two theme sources exist in the pinned checkout:
//! - `crates/ui/src/theme/default-theme.json` — the crate's own baseline
//!   pair, `"Default Light"` / `"Default Dark"`, both `is_default: true`.
//!   `gpui_component::init` loads these into a `ThemeRegistry` global and
//!   immediately applies `"Default Light"` (`theme::mod.rs::init`).
//! - a repo-root `themes/*.json` directory of 21 further theme families
//!   (Adventure, Alduin, ... Twilight). The crate's own `story` example app
//!   loads these via `ThemeRegistry::watch_dir`, which does real filesystem
//!   I/O against a `./themes` directory at runtime — exactly what the shell
//!   must not do on any path that can run during rendering (PHILOSOPHY.md:
//!   "nothing may stall the render thread").
//!
//! Neither set is reachable through a Cargo feature or an `include_str!`
//! friendly crate API, so per the brief both are vendored verbatim into
//! `assets/themes/` (22 files: `default.json` plus the 21 family files) and
//! embedded here with `include_str!` — no runtime file I/O, binary stays
//! self-contained.
//!
//! Parsing uses the crate's own config type, `gpui_component::ThemeSet` /
//! `ThemeConfig` (`crates/ui/src/theme/schema.rs`) — a theme JSON file is a
//! `ThemeSet` (a family: `name`, `author`, `url`, and a `themes: Vec<
//! ThemeConfig>` list, one `ThemeConfig` per mode the family ships). A
//! `ThemeConfig`'s own `.name` is already fully qualified, e.g. `"Gruvbox
//! Dark"`; the `ThemeSet`'s `.name` is the bare family, e.g. `"Gruvbox"`.
//!
//! Application goes through the crate's own two-call sequence — confirmed
//! against its own reference usage (`crates/story/src/themes.rs::
//! apply_theme_config`), not just the general "usage.md" skill note (which
//! names `apply_config` alone; at this pinned rev that skips the Base-layer
//! projection sync that `Theme::change` performs, so scrollbars/resize
//! handles would lag the new theme):
//! ```ignore
//! Theme::global_mut(cx).apply_config(&config); // sets colors/tokens/mode
//! Theme::change(config.mode, None, cx);        // syncs the Base projection
//! ```
//!
//! `ThemeRegistry` itself (the crate's own theme index) is deliberately not
//! used as our index: it is a gpui `Global`, reachable only via `cx: &mut
//! App`, which would make the parsing/lookup step untestable without a
//! window. `ThemeService` is its own `cx`-free bundled-theme index instead;
//! only the two `cx`-touching methods (`apply`, `apply_from_config`,
//! `toggle_mode`) reach into gpui at all.

use std::rc::Rc;

use gpui::App;
use gpui_component::{Theme, ThemeConfig, ThemeSet};

use geode_core::config::Config;

/// Re-exported so callers don't need a direct `gpui_component` dependency
/// just to name a mode.
pub use gpui_component::ThemeMode as Mode;

/// The family every bundled theme set falls back to: gpui-component's own
/// default, which always ships both a light and a dark variant.
pub const DEFAULT_FAMILY: &str = "Default";

/// One vendored theme JSON, embedded at compile time as `(label, json)`.
/// `label` is only used in parse-failure warnings — the real family name
/// comes from the parsed `ThemeSet::name`, so there is exactly one source of
/// truth for it. No runtime file I/O (spec: PHILOSOPHY.md "nothing may
/// stall the render thread").
const BUNDLED: &[(&str, &str)] = &[
    (
        "default",
        include_str!("../../../assets/themes/default.json"),
    ),
    (
        "adventure",
        include_str!("../../../assets/themes/adventure.json"),
    ),
    ("alduin", include_str!("../../../assets/themes/alduin.json")),
    (
        "asciinema",
        include_str!("../../../assets/themes/asciinema.json"),
    ),
    ("aurora", include_str!("../../../assets/themes/aurora.json")),
    ("ayu", include_str!("../../../assets/themes/ayu.json")),
    (
        "catppuccin",
        include_str!("../../../assets/themes/catppuccin.json"),
    ),
    (
        "everforest",
        include_str!("../../../assets/themes/everforest.json"),
    ),
    (
        "fahrenheit",
        include_str!("../../../assets/themes/fahrenheit.json"),
    ),
    (
        "flexoki",
        include_str!("../../../assets/themes/flexoki.json"),
    ),
    (
        "gruvbox",
        include_str!("../../../assets/themes/gruvbox.json"),
    ),
    ("harper", include_str!("../../../assets/themes/harper.json")),
    ("hybrid", include_str!("../../../assets/themes/hybrid.json")),
    (
        "jellybeans",
        include_str!("../../../assets/themes/jellybeans.json"),
    ),
    ("kibble", include_str!("../../../assets/themes/kibble.json")),
    (
        "macos-classic",
        include_str!("../../../assets/themes/macos-classic.json"),
    ),
    (
        "mellifluous",
        include_str!("../../../assets/themes/mellifluous.json"),
    ),
    (
        "molokai",
        include_str!("../../../assets/themes/molokai.json"),
    ),
    (
        "solarized",
        include_str!("../../../assets/themes/solarized.json"),
    ),
    (
        "spaceduck",
        include_str!("../../../assets/themes/spaceduck.json"),
    ),
    (
        "tokyonight",
        include_str!("../../../assets/themes/tokyonight.json"),
    ),
    (
        "twilight",
        include_str!("../../../assets/themes/twilight.json"),
    ),
];

/// The bundled-theme index plus whichever theme is currently active. Built
/// once via [`load_bundled`]; the app keeps one instance for the life of the
/// window (spec: `ShellServices`).
pub struct ThemeService {
    /// `(family, config)` pairs, in bundle order. Flat and possibly several
    /// entries per family (some families ship more than one dark variant,
    /// e.g. Tokyo Night/Storm/Moon) — deliberately not deduplicated down to
    /// one-per-mode, or those extra variants would be unreachable.
    entries: Vec<(String, Rc<ThemeConfig>)>,
    active_name: String,
    active_mode: Mode,
    active_family: String,
}

/// Parse every [`BUNDLED`] theme JSON. A file that fails to parse becomes a
/// warning string, never a crash (config philosophy) — its themes are just
/// absent from the resulting service. `ThemeService::names()` and
/// `resolve()` are meaningful even with zero warnings ignored: the returned
/// service always has at least the crate's own `"Default Light"`/`"Default
/// Dark"` pair, since `default.json` failing to parse would be a build-time
/// bug in this file, not a runtime condition to design around.
pub fn load_bundled() -> (ThemeService, Vec<String>) {
    let mut entries = Vec::new();
    let mut warnings = Vec::new();

    for (label, json) in BUNDLED {
        match serde_json::from_str::<ThemeSet>(json) {
            Ok(set) => {
                let family = set.name.to_string();
                for config in set.themes {
                    entries.push((family.clone(), Rc::new(config)));
                }
            }
            Err(err) => {
                warnings.push(format!("bundled theme '{label}' failed to parse: {err}"));
            }
        }
    }

    let service = ThemeService {
        entries,
        // Mirrors what `gpui_component::init` itself applies before
        // `apply_from_config` ever runs (`theme::mod.rs::init`:
        // `Theme::change(ThemeMode::Light, None, cx)`, and the registry's
        // default light theme is this same "Default Light").
        active_name: "Default Light".to_string(),
        active_mode: Mode::Light,
        active_family: DEFAULT_FAMILY.to_string(),
    };
    (service, warnings)
}

/// Normalize a theme family name for lookup (Task 6): `-` and `_` become
/// spaces, then the whole string is lowercased — so `"macos-classic"`,
/// `"macos_classic"`, and `"macOS Classic"` all compare equal. Used only
/// by [`ThemeService::find_family`]; [`ThemeService::find_exact`] still
/// matches a `ThemeConfig.name` byte-for-byte, since that's already a
/// known-good fully qualified name.
fn normalize_theme_name(name: &str) -> String {
    name.chars()
        .map(|c| if c == '-' || c == '_' { ' ' } else { c })
        .collect::<String>()
        .to_lowercase()
}

impl ThemeService {
    /// Every bundled theme's fully qualified name (e.g. `"Gruvbox Dark"`),
    /// sorted and deduplicated — the palette's theme-picker list.
    pub fn names(&self) -> Vec<String> {
        let mut names: Vec<String> = self
            .entries
            .iter()
            .map(|(_, c)| c.name.to_string())
            .collect();
        names.sort();
        names.dedup();
        names
    }

    /// The fully qualified name of the theme last successfully applied
    /// (`apply`, `apply_from_config`, or `toggle_mode`) — what the status
    /// bar shows.
    pub fn active_name(&self) -> &str {
        &self.active_name
    }

    /// Pure lookup, no `cx`: an exact `ThemeConfig.name` match wins outright
    /// (e.g. `"Gruvbox Dark"`, ignoring `mode` — a fully qualified name
    /// already says which mode it is); failing that, a case- and
    /// punctuation-insensitive family name (`-`/`_` normalize to a space
    /// before case-fold, Task 6 — see [`normalize_theme_name`]) combined
    /// with `mode` (e.g. `"Gruvbox"` + dark -> `"Gruvbox Dark"`, or
    /// `"macos-classic"` + light -> `"macOS Classic Light"`). `None` means
    /// "no such theme" — callers decide the default-theme fallback.
    pub fn resolve(&self, name: &str, mode: Mode) -> Option<&Rc<ThemeConfig>> {
        self.find_exact(name)
            .or_else(|| self.find_family(name, mode))
            .map(|(_, config)| config)
    }

    fn find_exact(&self, name: &str) -> Option<(&str, &Rc<ThemeConfig>)> {
        self.entries
            .iter()
            .find(|(_, config)| config.name.as_ref() == name)
            .map(|(family, config)| (family.as_str(), config))
    }

    /// Family-name lookup is punctuation- and case-insensitive: `-`/`_`
    /// normalize to a space before case-folding (Task 6), so a palette
    /// query like `"macos-classic"` resolves the same family as
    /// `"macOS Classic"` (the actual bundled name, `assets/themes/
    /// macos-classic.json`'s `ThemeSet.name`) or `"macos_classic"`.
    fn find_family(&self, family: &str, mode: Mode) -> Option<(&str, &Rc<ThemeConfig>)> {
        let target = normalize_theme_name(family);
        self.entries
            .iter()
            .find(|(fam, config)| normalize_theme_name(fam) == target && config.mode == mode)
            .map(|(fam, config)| (fam.as_str(), config))
    }

    /// Pure `[theme]` config resolution, no `cx`: what `apply_from_config`
    /// would apply, plus any warnings. `theme.mode` defaults to dark and
    /// `theme.name` to [`DEFAULT_FAMILY`] when absent; an unrecognized
    /// `theme.mode` or `theme.name` value falls back the same way, with a
    /// warning (config philosophy: bad input is a warning, never a crash).
    pub fn resolve_config(&self, config: &Config) -> (Option<&Rc<ThemeConfig>>, Vec<String>) {
        let mut warnings = Vec::new();

        let mode_raw = config.get("app", "theme.mode").and_then(|v| v.as_str());
        let mode = match mode_raw {
            Some("light") => Mode::Light,
            Some("dark") | None => Mode::Dark,
            Some(other) => {
                warnings.push(format!(
                    "theme.mode '{other}' is not 'light' or 'dark'; using dark"
                ));
                Mode::Dark
            }
        };

        let name = config.get("app", "theme.name").and_then(|v| v.as_str());
        let resolved = match name {
            Some(n) => self.resolve(n, mode).or_else(|| {
                warnings.push(format!("unknown theme '{n}'; using default"));
                self.resolve(DEFAULT_FAMILY, mode)
            }),
            None => self.resolve(DEFAULT_FAMILY, mode),
        };

        (resolved, warnings)
    }

    /// The mode the current `[theme]` config would resolve to right now —
    /// what [`resolve_config`](Self::resolve_config)/`apply_from_config`
    /// would apply, without applying it (fix wave, Fix 3: session
    /// theme-mode precedence needs to compare the *active* mode against
    /// this, not against `resolve_config`'s raw `theme.mode` parse, so an
    /// exact fully-qualified `theme.name` — e.g. `"Gruvbox Dark"`, which
    /// `resolve` matches outright regardless of `theme.mode` — is judged by
    /// the mode that name actually carries, the same mode that would land
    /// on screen, not by a separately-configured `theme.mode` value that
    /// resolution would have silently overridden). Falls back to the same
    /// light/dark default `resolve_config` uses on the (practically
    /// unreachable, since `DEFAULT_FAMILY` always ships both modes) chance
    /// nothing resolves at all.
    pub fn config_resolved_mode(&self, config: &Config) -> Mode {
        let (resolved, _warnings) = self.resolve_config(config);
        resolved.map(|c| c.mode).unwrap_or_else(|| {
            match config.get("app", "theme.mode").and_then(|v| v.as_str()) {
                Some("light") => Mode::Light,
                _ => Mode::Dark,
            }
        })
    }

    /// Apply a theme by exact or family name (see [`resolve`](Self::resolve)
    /// for the matching rule). Returns whether anything matched — an unknown
    /// name leaves the current theme untouched, mirroring the config
    /// philosophy at the call site (caller decides the fallback + warning).
    pub fn apply(&mut self, name: &str, mode: Mode, cx: &mut App) -> bool {
        let Some(config) = self.resolve(name, mode).cloned() else {
            return false;
        };
        self.apply_theme(&config, cx);
        true
    }

    /// Read `[theme] name`/`mode` from `config` and apply the result (see
    /// [`resolve_config`](Self::resolve_config)), falling back to
    /// [`DEFAULT_FAMILY`] at the resolved mode when nothing matches. Returns
    /// any warnings for the caller to surface (main.rs prints one line per
    /// warning, same as config/keymap diagnostics).
    pub fn apply_from_config(&mut self, config: &Config, cx: &mut App) -> Vec<String> {
        let (resolved, warnings) = self.resolve_config(config);
        if let Some(theme) = resolved.cloned() {
            self.apply_theme(&theme, cx);
        }
        warnings
    }

    /// The active theme's mode — what a session file records at save time
    /// (`session::SessionExtra::theme_mode`, Task 3) and what `main.rs`
    /// restores via [`set_mode`](Self::set_mode) after `apply_from_config`.
    pub fn active_mode(&self) -> Mode {
        self.active_mode
    }

    /// Flip light/dark for the active family (`theme::toggle_mode`). Stays
    /// on the same family when it ships both modes; when it doesn't (a
    /// single-mode family like "Harper"), falls back to [`DEFAULT_FAMILY`]
    /// at the target mode rather than doing nothing — a mode toggle should
    /// always visibly change something.
    pub fn toggle_mode(&mut self, cx: &mut App) {
        let target = if self.active_mode.is_dark() {
            Mode::Light
        } else {
            Mode::Dark
        };
        self.set_mode(target, cx);
    }

    /// Set the active family's mode directly (rather than flipping it —
    /// see [`toggle_mode`](Self::toggle_mode)). Used by session restore
    /// (`main.rs`, Task 3) to re-apply a saved mode *after*
    /// `apply_from_config` already applied the config's own `[theme]`
    /// mode — so the session's mode wins, on top of whatever family/mode
    /// the config resolved. A no-op when already at `mode`. Falls back to
    /// [`DEFAULT_FAMILY`] at `mode` when the active family doesn't ship it,
    /// same fallback `toggle_mode` uses.
    pub fn set_mode(&mut self, mode: Mode, cx: &mut App) {
        if self.active_mode == mode {
            return;
        }
        let family = self.active_family.clone();
        let next = self
            .find_family(&family, mode)
            .or_else(|| self.find_family(DEFAULT_FAMILY, mode))
            .map(|(_, config)| config.clone());
        if let Some(config) = next {
            self.apply_theme(&config, cx);
        }
    }

    fn apply_theme(&mut self, config: &Rc<ThemeConfig>, cx: &mut App) {
        Theme::global_mut(cx).apply_config(config);
        Theme::change(config.mode, None, cx);

        self.active_name = config.name.to_string();
        self.active_mode = config.mode;
        self.active_family = self
            .entries
            .iter()
            .find(|(_, c)| Rc::ptr_eq(c, config))
            .map(|(family, _)| family.clone())
            .unwrap_or_else(|| config.name.to_string());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use geode_core::config::{ConfigSources, LayerDoc};

    fn config_from(app_toml: &str) -> Config {
        let doc = LayerDoc::builtin("app", app_toml).unwrap();
        Config::load(&ConfigSources {
            builtin: vec![doc],
            desk: None,
            user: None,
        })
    }

    #[test]
    fn bundled_themes_all_parse_clean() {
        let (service, warnings) = load_bundled();
        assert!(warnings.is_empty(), "{warnings:?}");
        // 22 vendored files, several with more than one variant (e.g. Tokyo
        // Night ships 3 dark variants) — comfortably more than one-per-file.
        assert!(
            service.entries.len() >= BUNDLED.len(),
            "expected at least one ThemeConfig per bundled file, got {}",
            service.entries.len()
        );
    }

    #[test]
    fn names_lists_every_bundled_theme_sorted_and_deduplicated() {
        let (service, _) = load_bundled();
        let names = service.names();
        assert!(names.contains(&"Default Light".to_string()));
        assert!(names.contains(&"Default Dark".to_string()));
        assert!(names.contains(&"Gruvbox Dark".to_string()));
        assert!(names.contains(&"Tokyo Storm".to_string()));
        let mut sorted = names.clone();
        sorted.sort();
        assert_eq!(names, sorted);
        let mut deduped = names.clone();
        deduped.dedup();
        assert_eq!(names, deduped);
    }

    #[test]
    fn resolve_matches_an_exact_name_regardless_of_the_mode_argument() {
        let (service, _) = load_bundled();
        let found = service
            .resolve("Gruvbox Dark", Mode::Light)
            .expect("exact name should match even with a mismatched mode arg");
        assert_eq!(found.name.as_ref(), "Gruvbox Dark");
    }

    #[test]
    fn resolve_matches_a_family_name_combined_with_mode() {
        let (service, _) = load_bundled();
        assert_eq!(
            service
                .resolve("Gruvbox", Mode::Dark)
                .unwrap()
                .name
                .as_ref(),
            "Gruvbox Dark"
        );
        assert_eq!(
            service
                .resolve("gruvbox", Mode::Light)
                .unwrap()
                .name
                .as_ref(),
            "Gruvbox Light",
            "family lookup is case-insensitive"
        );
    }

    #[test]
    fn resolve_normalizes_dashes_and_underscores_to_spaces_in_family_lookup() {
        let (service, _) = load_bundled();
        // The bundled family name is "macOS Classic" (a space); Task 6:
        // dash/underscore variants of it must resolve the same theme.
        assert_eq!(
            service
                .resolve("macos-classic", Mode::Light)
                .unwrap()
                .name
                .as_ref(),
            "macOS Classic Light"
        );
        assert_eq!(
            service
                .resolve("macos_classic", Mode::Dark)
                .unwrap()
                .name
                .as_ref(),
            "macOS Classic Dark"
        );
        assert_eq!(
            service
                .resolve("MacOS-Classic", Mode::Dark)
                .unwrap()
                .name
                .as_ref(),
            "macOS Classic Dark",
            "normalization combines with the existing case-insensitivity"
        );
    }

    #[test]
    fn resolve_returns_none_for_an_unknown_name() {
        let (service, _) = load_bundled();
        assert!(service.resolve("not-a-real-theme", Mode::Dark).is_none());
    }

    #[test]
    fn resolve_config_defaults_to_default_dark_when_config_is_silent() {
        let (service, _) = load_bundled();
        let config = Config::load(&ConfigSources::default());
        let (resolved, warnings) = service.resolve_config(&config);
        assert!(warnings.is_empty());
        assert_eq!(resolved.unwrap().name.as_ref(), "Default Dark");
    }

    #[test]
    fn resolve_config_reads_theme_name_and_mode_from_config() {
        let (service, _) = load_bundled();
        let config = config_from("[theme]\nname = \"Gruvbox\"\nmode = \"light\"\n");
        let (resolved, warnings) = service.resolve_config(&config);
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(resolved.unwrap().name.as_ref(), "Gruvbox Light");
    }

    #[test]
    fn resolve_config_falls_back_to_default_with_a_warning_for_an_unknown_name() {
        let (service, _) = load_bundled();
        let config = config_from("[theme]\nname = \"not-a-real-theme\"\nmode = \"dark\"\n");
        let (resolved, warnings) = service.resolve_config(&config);
        assert_eq!(resolved.unwrap().name.as_ref(), "Default Dark");
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("not-a-real-theme"));
    }

    #[test]
    fn resolve_config_falls_back_to_dark_with_a_warning_for_an_invalid_mode() {
        let (service, _) = load_bundled();
        let config = config_from("[theme]\nmode = \"nocturnal\"\n");
        let (resolved, warnings) = service.resolve_config(&config);
        assert_eq!(resolved.unwrap().name.as_ref(), "Default Dark");
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("nocturnal"));
    }

    // --- config_resolved_mode (fix wave, Fix 3) --------------------------

    #[test]
    fn config_resolved_mode_defaults_to_dark_when_config_is_silent() {
        let (service, _) = load_bundled();
        let config = Config::load(&ConfigSources::default());
        assert_eq!(service.config_resolved_mode(&config), Mode::Dark);
    }

    #[test]
    fn config_resolved_mode_reads_theme_mode_from_config() {
        let (service, _) = load_bundled();
        let config = config_from("[theme]\nmode = \"light\"\n");
        assert_eq!(service.config_resolved_mode(&config), Mode::Light);
    }

    #[test]
    fn config_resolved_mode_follows_an_exact_fully_qualified_name_over_a_mismatched_mode() {
        // `theme.name = "Gruvbox Dark"` is an exact `ThemeConfig.name`
        // match (`resolve`'s `find_exact`, checked before family+mode
        // lookup), so it wins outright regardless of `theme.mode` here
        // saying light — the resolved mode must reflect what would
        // actually apply (dark), not the separately-configured raw
        // `theme.mode` value it silently overrides.
        let (service, _) = load_bundled();
        let config = config_from("[theme]\nname = \"Gruvbox Dark\"\nmode = \"light\"\n");
        assert_eq!(service.config_resolved_mode(&config), Mode::Dark);
    }
}

#[cfg(test)]
mod gpui_tests {
    use super::*;
    use geode_core::config::{ConfigSources, LayerDoc};
    use gpui::TestAppContext;

    fn config_from(app_toml: &str) -> Config {
        let doc = LayerDoc::builtin("app", app_toml).unwrap();
        Config::load(&ConfigSources {
            builtin: vec![doc],
            desk: None,
            user: None,
        })
    }

    #[gpui::test]
    fn apply_from_config_applies_the_resolved_theme_to_the_live_global(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let (mut service, _) = load_bundled();
        let config = config_from("[theme]\nname = \"Gruvbox\"\nmode = \"light\"\n");

        let warnings = cx.update(|cx| service.apply_from_config(&config, cx));

        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(service.active_name(), "Gruvbox Light");
        cx.update(|cx| {
            assert_eq!(
                gpui_component::Theme::global(cx).theme_name().as_ref(),
                "Gruvbox Light"
            );
        });
    }

    #[gpui::test]
    fn set_mode_applies_the_target_mode_within_the_active_family(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let (mut service, _) = load_bundled();
        cx.update(|cx| assert!(service.apply("Gruvbox", Mode::Dark, cx)));
        assert_eq!(service.active_mode(), Mode::Dark);

        cx.update(|cx| service.set_mode(Mode::Light, cx));
        assert_eq!(service.active_name(), "Gruvbox Light");
        assert_eq!(service.active_mode(), Mode::Light);
    }

    #[gpui::test]
    fn set_mode_to_the_current_mode_is_a_noop(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let (mut service, _) = load_bundled();
        cx.update(|cx| assert!(service.apply("Gruvbox", Mode::Dark, cx)));

        cx.update(|cx| service.set_mode(Mode::Dark, cx));
        assert_eq!(service.active_name(), "Gruvbox Dark");
    }

    #[gpui::test]
    fn toggle_mode_flips_light_and_dark_within_the_active_family(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let (mut service, _) = load_bundled();
        cx.update(|cx| assert!(service.apply("Gruvbox", Mode::Dark, cx)));
        assert_eq!(service.active_name(), "Gruvbox Dark");

        cx.update(|cx| service.toggle_mode(cx));
        assert_eq!(service.active_name(), "Gruvbox Light");

        cx.update(|cx| service.toggle_mode(cx));
        assert_eq!(service.active_name(), "Gruvbox Dark");
    }

    #[gpui::test]
    fn toggle_mode_falls_back_to_default_family_for_a_single_mode_theme(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let (mut service, _) = load_bundled();
        cx.update(|cx| assert!(service.apply("Harper", Mode::Dark, cx)));
        assert_eq!(service.active_name(), "Harper");

        cx.update(|cx| service.toggle_mode(cx));
        assert_eq!(
            service.active_name(),
            "Default Light",
            "Harper has no light variant, so toggling falls back to Default"
        );
    }
}
