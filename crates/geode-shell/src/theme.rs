//! Theming (Task 5): gpui-component's bundled theme JSONs, selectable from
//! config, the palette and the settings dialog. Geode is a lens, not a
//! brand exercise (PHILOSOPHY.md): theming stays entirely inside
//! gpui-component's own theme-config format, applied through its own theme
//! global — nothing here invents a raw color.
//!
//! ## Registry findings (gpui-component 0.6.2, pinned in the workspace
//! root `Cargo.toml`)
//!
//! Two theme sources exist in the pinned release:
//! - `gpui-component-0.6.2/src/theme/default-theme.json` — the crate's
//!   own baseline pair, `"Default Light"` / `"Default Dark"`, both
//!   `is_default: true`.
//!   `gpui_component::init` loads these into a `ThemeRegistry` global and
//!   immediately applies `"Default Light"` (`theme::mod.rs::init`).
//! - **upstream repo (`longbridge/gpui-kit`), not the registry crate:** a
//!   repo-root `themes/*.json` directory of 21 further theme families
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
//! Four further files are written for this repo rather than vendored, to the
//! same `ThemeSet` schema so none of them needs special handling here:
//! - `bloomberg.json` — two dark variants under one family, the Tokyo Night
//!   arrangement: `"Bloomberg"`, the classic Terminal palette (black ground,
//!   amber text, blue column heads), and `"Bloomberg Modern"`, the current
//!   look (near-black blue-grey ground, white text, orange as accent only).
//! - `modus.json` — `"Modus Operandi"`/`"Modus Vivendi"`, the one family
//!   here that ships a real light/dark pair. Its whole design constraint is a
//!   7:1 (WCAG AAA) floor on every information-bearing pairing; that floor
//!   is measured, not assumed — see the note below.
//! - `nord.json` — the canonical nord0–nord15 palette.
//! - `tradingview.json` — `"TradingView Dark"`, dark-only, named for the
//!   mode because the product ships a light theme too.
//!
//! Contrast: the two derived-tint colours in Nord and TradingView
//! (`muted.foreground`, `tab.foreground`) were raised off their first values
//! to clear 4.5:1, and Nord's `chart_bearish` uses a lighter tint of nord11
//! rather than nord11 itself, which sits at only 3.05:1 on nord0 — too weak
//! to encode a loss. Nord's `danger` fill keeps light-on-nord11 (3.55:1)
//! because that is what Nord itself does; it is chrome, not data.
//!
//! Parsing uses the crate's own config type, `gpui_component::ThemeSet` /
//! `ThemeConfig` (`gpui-component-0.6.2/src/theme/schema.rs`) — a theme
//! JSON file is a `ThemeSet` (a family: `name`, `author`, `url`, and a
//! `themes: Vec<
//! ThemeConfig>` list, one `ThemeConfig` per variant the family ships). A
//! `ThemeConfig`'s own `.name` is already fully qualified, e.g. `"Gruvbox
//! Dark"`; the `ThemeSet`'s `.name` is the bare family, e.g. `"Gruvbox"`.
//!
//! ## No light/dark mode
//!
//! There is no mode axis (user ruling 2026-09-12): Geode has themes, some
//! of which are light and some dark, and a theme's name says which. The
//! family is a file-level grouping only — `"Gruvbox"` is not a theme and
//! resolves to nothing; `"Gruvbox Light"` and `"Gruvbox Dark"` are two
//! themes. The `theme::toggle_mode` action, its `mod+shift+t` chord, the
//! settings dialog's Dark mode row and the `[theme] mode` config key were
//! all retired with it; a `mode` key still present in a config file is
//! ignored with a warning, and the next persisted theme pick removes it.
//! `ThemeConfig.mode` itself stays, because gpui-component reads it for
//! its own Base-layer projection — it is a property of the theme, not a
//! choice a trader makes.
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
//! only the two `cx`-touching methods (`apply`, `apply_from_config`)
//! reach into gpui at all.
//!
//! ## Persistence
//!
//! Every UI theme change (settings dialog, palette theme pick) is applied
//! live through this service, then persisted into the user config layer
//! by [`persist_to_user_config`] — see its own doc comment for the full
//! contract, and `ShellView::persist_theme` (`shell/input.rs`) for the
//! one seam both UI paths call through. There is no longer a session-file theme mechanism: a
//! theme choice is ordinary config, resolved by the same builtin → desk →
//! user merge (`geode_core::config`) as everything else.

use std::path::Path;
use std::rc::Rc;

use gpui::App;
use gpui_component::{Theme, ThemeConfig, ThemeSet};
use toml_edit::{Item, Table, value};

use geode_core::config::{Config, Layer};

/// The theme applied when config names none, or names one that does not
/// exist: gpui-component's own default family, at its dark variant.
pub const DEFAULT_THEME: &str = "Default Dark";

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
    (
        "bloomberg",
        include_str!("../../../assets/themes/bloomberg.json"),
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
    ("modus", include_str!("../../../assets/themes/modus.json")),
    ("nord", include_str!("../../../assets/themes/nord.json")),
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
        "tradingview",
        include_str!("../../../assets/themes/tradingview.json"),
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
    /// Every bundled `ThemeConfig`, in bundle order. Flat: a family that
    /// ships several variants (Tokyo Night/Storm/Moon, Gruvbox Light and
    /// Dark) contributes one entry each, and nothing here groups them
    /// back into families — a theme is one named entry (see the module
    /// doc's "No light/dark mode").
    entries: Vec<Rc<ThemeConfig>>,
    active_name: String,
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
                for config in set.themes {
                    entries.push(Rc::new(config));
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
    };
    (service, warnings)
}

/// Normalize a theme name for lookup (Task 6): `-` and `_` become spaces,
/// then the whole string is lowercased — so `"macos-classic-light"`,
/// `"macos_classic_light"`, and `"macOS Classic Light"` all compare equal.
/// [`ThemeService::resolve`]'s second, forgiving pass; its first pass
/// still matches a `ThemeConfig.name` byte-for-byte.
fn normalize_theme_name(name: &str) -> String {
    name.chars()
        .map(|c| if c == '-' || c == '_' { ' ' } else { c })
        .collect::<String>()
        .to_lowercase()
}

impl ThemeService {
    /// Every bundled theme's fully qualified name (e.g. `"Gruvbox Dark"`),
    /// sorted and deduplicated — the palette's theme-picker list and the
    /// settings dialog's Theme row.
    pub fn names(&self) -> Vec<String> {
        let mut names: Vec<String> = self.entries.iter().map(|c| c.name.to_string()).collect();
        names.sort();
        names.dedup();
        names
    }

    /// The fully qualified name of the theme last successfully applied
    /// (`apply` or `apply_from_config`) — what the status bar shows.
    pub fn active_name(&self) -> &str {
        &self.active_name
    }

    /// Pure lookup, no `cx`: an exact `ThemeConfig.name` match wins
    /// outright (`"Gruvbox Dark"`); failing that, a case- and
    /// punctuation-insensitive match on the same full name
    /// (`"gruvbox-dark"`, `"macos_classic_light"` — see
    /// [`normalize_theme_name`]). A bare family name (`"Gruvbox"`) is not
    /// a theme and resolves to nothing: there is no light/dark axis to
    /// complete it with (module doc, "No light/dark mode"), so a config
    /// must name the variant it means. `None` means "no such theme" —
    /// callers decide the fallback.
    pub fn resolve(&self, name: &str) -> Option<&Rc<ThemeConfig>> {
        self.entries
            .iter()
            .find(|c| c.name.as_ref() == name)
            .or_else(|| {
                let target = normalize_theme_name(name);
                self.entries
                    .iter()
                    .find(|c| normalize_theme_name(&c.name) == target)
            })
    }

    /// Pure `[theme]` config resolution, no `cx`: what `apply_from_config`
    /// would apply, plus any warnings. `theme.name` defaults to
    /// [`DEFAULT_THEME`] when absent, and an unrecognized name falls back
    /// to it with a warning (config philosophy: bad input is a warning,
    /// never a crash). A `theme.mode` key — the retired light/dark axis —
    /// is ignored with a warning that names the replacement, since the
    /// theme's own name is what carries it now; `persist_to_user_config`
    /// removes the key on the next theme pick, so the warning is
    /// self-clearing.
    pub fn resolve_config(&self, config: &Config) -> (Option<&Rc<ThemeConfig>>, Vec<String>) {
        let mut warnings = Vec::new();

        if config.get("app", "theme.mode").is_some() {
            warnings.push(
                "theme.mode is retired and ignored: a theme's name already says whether it \
                 is light or dark (e.g. \"Gruvbox Light\"); set theme.name to the variant you want"
                    .to_string(),
            );
        }

        let name = config.get("app", "theme.name").and_then(|v| v.as_str());
        let resolved = match name {
            Some(n) => self.resolve(n).or_else(|| {
                warnings.push(format!("unknown theme '{n}'; using {DEFAULT_THEME}"));
                self.resolve(DEFAULT_THEME)
            }),
            None => self.resolve(DEFAULT_THEME),
        };

        (resolved, warnings)
    }

    /// Apply a theme by name (see [`resolve`](Self::resolve) for the
    /// matching rule). Returns whether anything matched — an unknown name
    /// leaves the current theme untouched, mirroring the config philosophy
    /// at the call site (caller decides the fallback + warning).
    pub fn apply(&mut self, name: &str, cx: &mut App) -> bool {
        let Some(config) = self.resolve(name).cloned() else {
            return false;
        };
        self.apply_theme(&config, cx);
        true
    }

    /// Read `[theme] name` from `config` and apply the result (see
    /// [`resolve_config`](Self::resolve_config)), falling back to
    /// [`DEFAULT_THEME`] when nothing matches. Returns any warnings for
    /// the caller to surface (main.rs prints one line per warning, same as
    /// config/keymap diagnostics).
    pub fn apply_from_config(&mut self, config: &Config, cx: &mut App) -> Vec<String> {
        let (resolved, warnings) = self.resolve_config(config);
        if let Some(theme) = resolved.cloned() {
            self.apply_theme(&theme, cx);
        }
        warnings
    }

    fn apply_theme(&mut self, config: &Rc<ThemeConfig>, cx: &mut App) {
        Theme::global_mut(cx).apply_config(config);
        // `config.mode` is the theme's own light/dark character, which
        // gpui-component needs for its Base-layer projection — a property
        // of the theme, not a mode a trader chose (Geode has none).
        Theme::change(config.mode, None, cx);
        self.active_name = config.name.to_string();
    }
}

/// Persist a theme choice into the user config layer (`<user_dir>/app.toml`,
/// `[theme] name`), so it survives a restart via the ordinary desk/user
/// config merge (`geode_core::config`) rather than a session file — the
/// settings dialog and palette theme picks both call this right after
/// applying the change live (`ShellView::persist_theme`, `shell/input.rs`).
/// A `mode` key left in `[theme]` from before the light/dark axis was
/// retired (module doc) is removed by the same write, so the load-time
/// "theme.mode is retired" warning clears itself on the first theme pick.
///
/// `toml_edit` — not the plain `toml` crate already used elsewhere in this
/// codebase (`session.rs`, `geode_core::config`) — is the whole reason this
/// function exists in this shape: a hand-written `app.toml` can carry
/// comments and unrelated keys/tables this write knows nothing about, and
/// only a format-preserving editor can update just `[theme]` without
/// clobbering any of that. Parsing with plain `toml::Table` and
/// re-serializing would silently destroy every comment and reorder every
/// key on the very first theme change — exactly the failure mode this
/// dependency exists to avoid.
///
/// - **Missing file**: created fresh, with `config_version = 1` at the top
///   (matching every other config document in this codebase) followed by
///   `[theme]`.
/// - **Existing, valid file**: `[theme]` is created if absent; `name` is
///   set (or overwritten) inside it and a stale `mode` removed. Everything
///   else — every other table, key, and comment — is byte-preserved by
///   `toml_edit`.
/// - **Existing, unparseable file**: returns `Err` without touching the
///   file at all. A user's hand-edited config, however broken, must never
///   be destroyed by a UI-driven write; the caller turns this into a
///   `[theme] warning:` stderr line, same convention as every other
///   config-write failure in this codebase, never a crash.
///
/// All three of those behaviours are [`crate::config_write::edit`]'s, not
/// this function's own: the missing-file stamp, the comment-preserving
/// `toml_edit` round trip, and the untouched-on-parse-failure refusal now
/// have exactly one implementation, shared with every other persist in
/// this crate. This function is only the `[theme]` half. The write itself
/// is atomic — a unique temp file in `user_dir`, an `fsync`, then a
/// rename — and the temp filename ends in `.tmp`, not `.toml`, so the
/// reload watcher's `*.toml` glob (`reload::scan`) never sees a partial
/// write mid-flight.
///
/// Hot-reload interplay (corrected, Finding 3 of review fix round 1 — the
/// previous wording here overclaimed a guard that doesn't exist): this
/// write lands inside a directory the reload watcher polls, so it WILL be
/// picked up as "config changed" on the watcher's next ~500ms tick and
/// trigger a reload. `ShellView::apply_reload` decides whether to re-apply
/// the theme by comparing its own *stale in-memory* `Config`'s `[theme]`
/// table (never updated by this function — it only ever touches the file
/// on disk) against the freshly reloaded one — and since this write really
/// did just change the on-disk `[theme]` table relative to that stale
/// in-memory copy, that first post-write reload's comparison DOES read as
/// changed and DOES call `apply_from_config` again. That is redundant, not
/// skipped: it re-applies a theme that's already active in `ThemeService`
/// (matching the same name this function just wrote), so it is still
/// harmless — merely idempotent in effect, not guarded away in mechanism.
/// (Finding 2 of the same review, separately: this reload must also not
/// close an open palette on a theme-only change — see `ShellView::
/// apply_reload`'s own `palette_snapshot_changed` check.) Accepted as-is:
/// deliberately no self-write suppression here, so this stays one plain
/// write path with no special-casing of its own output.
pub fn persist_to_user_config(user_dir: &Path, name: &str) -> Result<(), String> {
    crate::config_write::edit(user_dir, Layer::User, "app", |doc| {
        if !doc.get("theme").is_some_and(Item::is_table_like) {
            doc["theme"] = Item::Table(Table::new());
        }
        let theme_table = doc["theme"]
            .as_table_mut()
            .expect("just ensured [theme] is a table");
        theme_table["name"] = value(name);
        theme_table.remove("mode");
    })
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
        // 26 files (22 vendored plus the 4 written here), several with more
        // than one variant (e.g. Tokyo Night ships 3 dark variants) —
        // comfortably more than one-per-file.
        assert!(
            service.entries.len() >= BUNDLED.len(),
            "expected at least one ThemeConfig per bundled file, got {}",
            service.entries.len()
        );
    }

    #[test]
    fn modus_ships_both_a_light_and_a_dark_theme() {
        let (service, warnings) = load_bundled();
        assert!(warnings.is_empty(), "{warnings:?}");
        let light = service.resolve("Modus Operandi").unwrap();
        assert!(!light.mode.is_dark());
        let dark = service.resolve("Modus Vivendi").unwrap();
        assert!(dark.mode.is_dark());
        assert!(
            service.resolve("Modus").is_none(),
            "the family is a file-level grouping, not a theme"
        );
    }

    #[test]
    fn nord_and_tradingview_are_bundled_dark_only() {
        let (service, warnings) = load_bundled();
        assert!(warnings.is_empty(), "{warnings:?}");
        let nord = service
            .resolve("nord")
            .expect("lookup is case-insensitive on the full name");
        assert_eq!(nord.name.as_ref(), "Nord");
        assert!(nord.mode.is_dark());
        // Named "TradingView Dark" rather than bare (the product ships a
        // light theme too), so only the full name resolves it.
        let tv = service.resolve("TradingView Dark").unwrap();
        assert!(tv.mode.is_dark());
        assert!(service.resolve("TradingView").is_none());
    }

    #[test]
    fn bloomberg_is_bundled_as_a_dark_only_family() {
        let (service, warnings) = load_bundled();
        assert!(warnings.is_empty(), "{warnings:?}");
        assert!(service.names().contains(&"Bloomberg".to_string()));
        // Lookup normalizes case (see `normalize_theme_name`), so a config
        // `theme.name = "bloomberg"` and a palette pick of the display
        // name land on the same theme.
        let found = service.resolve("bloomberg").unwrap();
        assert_eq!(found.name.as_ref(), "Bloomberg");
        assert!(found.mode.is_dark());
    }

    #[test]
    fn bloomberg_ships_a_second_dark_variant_for_the_modern_terminal() {
        let (service, warnings) = load_bundled();
        assert!(warnings.is_empty(), "{warnings:?}");
        // Two dark variants in one family, the Tokyo Night arrangement
        // (three darks under "Tokyo Night") — `entries` is flat, so both
        // stay reachable by their own names.
        assert!(service.names().contains(&"Bloomberg Modern".to_string()));
        let modern = service
            .resolve("Bloomberg Modern")
            .expect("the modern variant resolves by its fully qualified name");
        assert!(modern.mode.is_dark());
        assert_eq!(
            service.resolve("Bloomberg").unwrap().name.as_ref(),
            "Bloomberg"
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
    fn resolve_matches_an_exact_name() {
        let (service, _) = load_bundled();
        let found = service.resolve("Gruvbox Dark").unwrap();
        assert_eq!(found.name.as_ref(), "Gruvbox Dark");
    }

    #[test]
    fn resolve_is_case_and_punctuation_insensitive_on_the_full_name() {
        let (service, _) = load_bundled();
        assert_eq!(
            service.resolve("gruvbox light").unwrap().name.as_ref(),
            "Gruvbox Light"
        );
        // The bundled name is "macOS Classic Light" (spaces); Task 6:
        // dash/underscore spellings must resolve the same theme.
        assert_eq!(
            service
                .resolve("macos-classic-light")
                .unwrap()
                .name
                .as_ref(),
            "macOS Classic Light"
        );
        assert_eq!(
            service.resolve("macos_classic_dark").unwrap().name.as_ref(),
            "macOS Classic Dark"
        );
        assert_eq!(
            service.resolve("MacOS-Classic Dark").unwrap().name.as_ref(),
            "macOS Classic Dark",
            "normalization combines with the case-insensitivity"
        );
    }

    #[test]
    fn a_bare_family_name_is_not_a_theme() {
        // User ruling 2026-09-12: there is no mode to complete "Gruvbox"
        // with, so it resolves to nothing rather than to a guessed variant.
        let (service, _) = load_bundled();
        assert!(service.resolve("Gruvbox").is_none());
        assert!(service.resolve("macos-classic").is_none());
        assert!(service.resolve("Default").is_none());
    }

    #[test]
    fn resolve_returns_none_for_an_unknown_name() {
        let (service, _) = load_bundled();
        assert!(service.resolve("not-a-real-theme").is_none());
    }

    #[test]
    fn resolve_config_defaults_to_default_dark_when_config_is_silent() {
        let (service, _) = load_bundled();
        let config = Config::load(&ConfigSources::default());
        let (resolved, warnings) = service.resolve_config(&config);
        assert!(warnings.is_empty());
        assert_eq!(resolved.unwrap().name.as_ref(), DEFAULT_THEME);
    }

    #[test]
    fn resolve_config_reads_theme_name_from_config() {
        let (service, _) = load_bundled();
        let config = config_from("[theme]\nname = \"Gruvbox Light\"\n");
        let (resolved, warnings) = service.resolve_config(&config);
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(resolved.unwrap().name.as_ref(), "Gruvbox Light");
    }

    #[test]
    fn resolve_config_falls_back_to_default_with_a_warning_for_an_unknown_name() {
        let (service, _) = load_bundled();
        let config = config_from("[theme]\nname = \"not-a-real-theme\"\n");
        let (resolved, warnings) = service.resolve_config(&config);
        assert_eq!(resolved.unwrap().name.as_ref(), DEFAULT_THEME);
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("not-a-real-theme"));
    }

    #[test]
    fn resolve_config_treats_a_bare_family_name_as_unknown() {
        // The pre-ruling shape, `name = "Gruvbox"` completed by `mode`:
        // now a warning and the default, never a silently guessed variant.
        let (service, _) = load_bundled();
        let config = config_from("[theme]\nname = \"Gruvbox\"\n");
        let (resolved, warnings) = service.resolve_config(&config);
        assert_eq!(resolved.unwrap().name.as_ref(), DEFAULT_THEME);
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("Gruvbox"));
    }

    #[test]
    fn resolve_config_warns_that_theme_mode_is_retired_and_ignores_it() {
        let (service, _) = load_bundled();
        let config = config_from("[theme]\nname = \"Gruvbox Light\"\nmode = \"dark\"\n");
        let (resolved, warnings) = service.resolve_config(&config);
        assert_eq!(
            resolved.unwrap().name.as_ref(),
            "Gruvbox Light",
            "the name decides; a contradicting mode key changes nothing"
        );
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(warnings[0].contains("theme.mode"));
        assert!(warnings[0].contains("retired"));
    }
}

/// [`persist_to_user_config`] tests: toml_edit round-trip preservation,
/// create-if-missing, and corrupt-file handling (design doc, "Tests" —
/// TDD the pure parts).
#[cfg(test)]
mod persist_tests {
    use super::*;

    #[test]
    fn creates_a_fresh_file_with_config_version_and_theme() {
        let dir = tempfile::tempdir().unwrap();
        persist_to_user_config(dir.path(), "Gruvbox Dark").unwrap();

        let text = std::fs::read_to_string(dir.path().join("app.toml")).unwrap();
        let doc: toml_edit::DocumentMut = text.parse().unwrap();
        assert_eq!(doc["config_version"].as_integer(), Some(1));
        assert_eq!(doc["theme"]["name"].as_str(), Some("Gruvbox Dark"));
        assert!(
            doc["theme"].get("mode").is_none(),
            "there is no mode to write"
        );
    }

    #[test]
    fn round_trips_an_existing_file_byte_preserving_comments_and_unrelated_keys() {
        let dir = tempfile::tempdir().unwrap();
        let original = "\
# a hand-written config
config_version = 1

# keymap comment stays put
[keymap]
mod = \"ctrl\" # inline comment

[theme]
# old theme comment
name = \"Default Light\"
";
        std::fs::write(dir.path().join("app.toml"), original).unwrap();

        persist_to_user_config(dir.path(), "Gruvbox Dark").unwrap();

        let text = std::fs::read_to_string(dir.path().join("app.toml")).unwrap();
        assert!(text.contains("# a hand-written config"));
        assert!(text.contains("# keymap comment stays put"));
        assert!(text.contains("mod = \"ctrl\" # inline comment"));
        assert!(text.contains("# old theme comment"));

        let doc: toml_edit::DocumentMut = text.parse().unwrap();
        assert_eq!(doc["theme"]["name"].as_str(), Some("Gruvbox Dark"));
        assert_eq!(
            doc["keymap"]["mod"].as_str(),
            Some("ctrl"),
            "unrelated [keymap] table must be untouched"
        );
    }

    #[test]
    fn a_stale_mode_key_is_removed_and_its_neighbours_kept() {
        // A file written before the light/dark axis was retired: the
        // write that follows the first theme pick cleans the key, and
        // only the key.
        let dir = tempfile::tempdir().unwrap();
        let original = "\
config_version = 1

[theme]
name = \"Default Light\"
mode = \"light\"

[ui]
font_size = \"large\"
";
        std::fs::write(dir.path().join("app.toml"), original).unwrap();

        persist_to_user_config(dir.path(), "Gruvbox Dark").unwrap();

        let text = std::fs::read_to_string(dir.path().join("app.toml")).unwrap();
        let doc: toml_edit::DocumentMut = text.parse().unwrap();
        assert_eq!(doc["theme"]["name"].as_str(), Some("Gruvbox Dark"));
        assert!(doc["theme"].get("mode").is_none(), "{text}");
        assert_eq!(doc["ui"]["font_size"].as_str(), Some("large"));
    }

    #[test]
    fn creates_the_theme_table_when_missing_from_an_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("app.toml"),
            "config_version = 1\n[keymap]\nmod = \"cmd\"\n",
        )
        .unwrap();

        persist_to_user_config(dir.path(), "Default Light").unwrap();

        let text = std::fs::read_to_string(dir.path().join("app.toml")).unwrap();
        let doc: toml_edit::DocumentMut = text.parse().unwrap();
        assert_eq!(doc["theme"]["name"].as_str(), Some("Default Light"));
        assert_eq!(
            doc["keymap"]["mod"].as_str(),
            Some("cmd"),
            "existing content must survive [theme] being newly added"
        );
        assert!(
            !text.contains("config_version = 1\nconfig_version"),
            "config_version must not be duplicated when the file already had one"
        );
    }

    #[test]
    fn a_corrupt_existing_file_is_left_untouched_and_returns_err() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("app.toml");
        let corrupt = "this is [not valid toml";
        std::fs::write(&path, corrupt).unwrap();

        let result = persist_to_user_config(dir.path(), "Gruvbox Dark");
        assert!(result.is_err(), "a parse failure must return Err");

        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(text, corrupt, "a corrupt file must never be written to");

        let leftover_tmp: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().is_some_and(|ext| ext == "tmp"))
            .collect();
        assert!(
            leftover_tmp.is_empty(),
            "no temp file should be left behind on a parse failure"
        );
    }

    #[test]
    fn overwriting_an_existing_theme_replaces_the_name() {
        let dir = tempfile::tempdir().unwrap();
        persist_to_user_config(dir.path(), "Gruvbox Dark").unwrap();
        persist_to_user_config(dir.path(), "Default Light").unwrap();

        let text = std::fs::read_to_string(dir.path().join("app.toml")).unwrap();
        let doc: toml_edit::DocumentMut = text.parse().unwrap();
        assert_eq!(doc["theme"]["name"].as_str(), Some("Default Light"));
    }

    #[test]
    fn no_leftover_tmp_files_after_a_successful_write() {
        let dir = tempfile::tempdir().unwrap();
        persist_to_user_config(dir.path(), "Gruvbox Dark").unwrap();

        let leftover_tmp: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().is_some_and(|ext| ext == "tmp"))
            .collect();
        assert!(leftover_tmp.is_empty(), "{leftover_tmp:?}");
    }
}

#[cfg(test)]
mod gpui_tests {
    use super::*;
    use geode_core::colour::Sign;
    use geode_core::config::{ConfigSources, LayerDoc};
    use gpui::TestAppContext;
    use gpui_component::ActiveTheme as _;

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
        let config = config_from("[theme]\nname = \"Gruvbox Light\"\n");

        let warnings = cx.update(|cx| service.apply_from_config(&config, cx));

        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(service.active_name(), "Gruvbox Light");
        cx.update(|cx| {
            assert_eq!(
                gpui_component::Theme::global(cx).theme_name().as_ref(),
                "Gruvbox Light"
            );
            assert!(
                !gpui_component::Theme::global(cx).mode.is_dark(),
                "the theme's own mode reaches gpui-component"
            );
        });
    }

    #[gpui::test]
    fn apply_by_a_normalized_name_activates_the_exact_theme(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let (mut service, _) = load_bundled();
        cx.update(|cx| assert!(service.apply("gruvbox-dark", cx)));
        assert_eq!(service.active_name(), "Gruvbox Dark");
        cx.update(|cx| {
            assert!(gpui_component::Theme::global(cx).mode.is_dark());
        });
    }

    #[gpui::test]
    fn apply_refuses_an_unknown_or_bare_family_name_and_keeps_the_theme(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let (mut service, _) = load_bundled();
        cx.update(|cx| assert!(service.apply("Gruvbox Light", cx)));

        cx.update(|cx| assert!(!service.apply("not-a-real-theme", cx)));
        assert_eq!(service.active_name(), "Gruvbox Light");

        cx.update(|cx| assert!(!service.apply("Gruvbox", cx)));
        assert_eq!(
            service.active_name(),
            "Gruvbox Light",
            "a family name is not a theme and must not pick a variant"
        );
    }

    /// 2c §7 fix round 2 (controller ruling, 2026-09-13): the exception
    /// list from fix round 1 is gone. [`geode_core::colour::resolve`]
    /// itself now floors every `Base::Hue` against the theme's
    /// own background (`readable_on`, spec §2.2/§7), so every bundled
    /// theme clears `READABLE_RATIO` at every generated hue, in both
    /// tones, with no exceptions — asserted here unconditionally, the
    /// same assertion for `Tone::Normal` and `Tone::Light` alike (the
    /// prior round's Light-tone report-only split is also gone: a
    /// floored resolver has nothing left to report there that the
    /// assertion doesn't already cover).
    ///
    /// What used to be a 107-pair exception list is now one number per
    /// theme: how many of the 24 (hue, tone) pairs needed the floor at
    /// all, found by comparing `resolve` against the raw, unfloored
    /// `interpolate_hue`. The five themes needing it most are
    /// `eprintln!`'d as the retune work order — as data, not a list
    /// committed to source, since a maintainer who improves a theme's
    /// own anchors changes that count on the next run rather than
    /// editing a line here.
    ///
    /// The anchor-arc report stays as an independent number: a folded
    /// palette (two anchors landing on the same OKLCH hue) is a
    /// different defect from a contrast failure, and Hybrid Dark
    /// supplies a live example — its smallest arc measures exactly
    /// 0.0°, i.e. two of its Normal-tone anchors resolve to the
    /// identical hue angle.
    #[gpui::test]
    fn every_bundled_theme_keeps_generated_hues_readable(cx: &mut gpui::TestAppContext) {
        cx.update(gpui_component::init);
        let (service, _) = load_bundled();
        let mut worst: Vec<(String, f32)> = Vec::new();
        let mut floor_counts: Vec<(String, u32)> = Vec::new();
        for entry in &service.entries {
            let (anchors, tokens) = cx.update(|cx| {
                Theme::global_mut(cx).apply_config(entry);
                let theme = cx.theme();
                (
                    crate::shell::colours::anchors_from_theme(theme),
                    crate::shell::colours::tokens_from_theme(theme),
                )
            });
            let mut smallest_arc = f32::MAX;
            for i in 0..6 {
                let a = geode_core::colour::oklab::lab_to_lch(
                    geode_core::colour::oklab::srgb_to_oklab(anchors.normal[i]),
                );
                let b = geode_core::colour::oklab::lab_to_lch(
                    geode_core::colour::oklab::srgb_to_oklab(anchors.normal[(i + 1) % 6]),
                );
                let arc = ((b.h - a.h + std::f32::consts::PI).rem_euclid(std::f32::consts::TAU)
                    - std::f32::consts::PI)
                    .abs()
                    .to_degrees();
                smallest_arc = smallest_arc.min(arc);
            }
            worst.push((entry.name.to_string(), smallest_arc));
            let mut floored = 0u32;
            for tone in [
                geode_core::colour::Tone::Normal,
                geode_core::colour::Tone::Light,
            ] {
                for step in 0..12 {
                    let hue = step as f32 * 30.0;
                    let raw = geode_core::colour::interpolate_hue(hue, tone, &anchors);
                    let def = geode_core::colour::Definition::hue(hue, tone);
                    let resolved = geode_core::colour::resolve(&def, &anchors, &tokens);
                    if resolved != raw {
                        floored += 1;
                    }
                    let ratio = geode_core::colour::contrast_ratio(resolved, tokens.background);
                    assert!(
                        ratio >= geode_core::colour::READABLE_RATIO,
                        "{}: hue {} ({tone:?}) reads {ratio:.2}:1 against the background even through the floor",
                        entry.name,
                        step * 30
                    );
                    // The two sign-tinted variants of the same hue are
                    // generated colours too, under the same floor.
                    for sign in [Sign::Negative, Sign::Positive] {
                        let tinted = geode_core::colour::resolve_signed(
                            &def.clone().tinted(),
                            sign,
                            &anchors,
                            &tokens,
                        );
                        let ratio = geode_core::colour::contrast_ratio(tinted, tokens.background);
                        assert!(
                            ratio >= geode_core::colour::READABLE_RATIO,
                            "{}: hue {} ({tone:?}) tinted {sign:?} reads {ratio:.2}:1 against the background",
                            entry.name,
                            step * 30
                        );
                    }
                }
            }
            floor_counts.push((entry.name.to_string(), floored));
            // A token's tinted variants: a base token is never floored
            // (it is the theme author's own colour) but its rotation is
            // ours, so every token on every theme must clear 3:1 once
            // tinted — with no exception list.
            for token in geode_core::colour::Token::ALL {
                let def = geode_core::colour::Definition::token(token).tinted();
                for sign in [Sign::Negative, Sign::Positive] {
                    let tinted = geode_core::colour::resolve_signed(&def, sign, &anchors, &tokens);
                    let ratio = geode_core::colour::contrast_ratio(tinted, tokens.background);
                    assert!(
                        ratio >= geode_core::colour::READABLE_RATIO,
                        "{}: token {} tinted {sign:?} reads {ratio:.2}:1 against the background",
                        entry.name,
                        token.name()
                    );
                }
            }
        }
        worst.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap());
        eprintln!("smallest anchor arcs: {:?}", &worst[..worst.len().min(5)]);

        floor_counts.sort_by_key(|(_, count)| std::cmp::Reverse(*count));
        eprintln!(
            "themes needing the readability floor most, of 24 hue/tone pairs each (retune work order): {:?}",
            &floor_counts[..floor_counts.len().min(5)]
        );
    }
}
