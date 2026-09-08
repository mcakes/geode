//! The config dialogs' shared scaffold: browse a config domain's named
//! objects, and (Task 5) edit one
//! (`docs/superpowers/specs/2026-09-08-geode-phase-4c-config-dialogs-design.md`).
//!
//! Phase 4c replaced an earlier design that would have put a TOML text
//! editor in a tile. Instead every config domain — views, groupings,
//! scopes, sources, the read-only schema — gets a purpose-built dialog
//! on this one scaffold, so a trader edits *objects with fields* rather
//! than text, and so the layer a change lands in is a property of the
//! scaffold rather than a thing each dialog remembers to get right.
//! This task builds the first stage, browse, over the first adapter,
//! [`Domain::Views`] — the hardest shape first, deliberately (spec §14),
//! so the vocabulary is settled before three thin adapters depend on it.
//!
//! ## Layout
//!
//! Split the way `keybindings_view` and `picker` are, one directory
//! further out because a domain adapter is a file of its own (spec §4):
//!
//! - this module — the pure core: [`Stage`], [`ObjectDialogState`],
//!   [`ObjectRow`], [`Domain`], and the one derivation of `layer` and
//!   `overridden` every domain shares;
//! - [`views`] — the `Domain::Views` adapter: the doc it reads and the
//!   one-line summary a view row shows, and nothing else;
//! - [`render`] — the gpui shell: `open`, the [`dialog::ModalKeyHandler`],
//!   and the painted list.
//!
//! No `gpui` type appears in this file, so every transition and every
//! marker below is unit-testable without a window — the same split, for
//! the same reason, that keeps `KeybindingsState` free of the scroll
//! handle that sits beside it on `ShellView`.
//!
//! ## Rows are derived, never cached
//!
//! [`Domain::objects`] runs fresh on every render and every keystroke.
//! That is the contract `keybindings_view::derive_rows` and
//! `settings_view::derive_rows` both hold, and the reason those dialogs
//! cannot show a stale value: a config reload lands in
//! `ShellView::services.config` with no notification to any dialog, so
//! anything cached here would be wrong from the next 500 ms watcher tick
//! onward. Only a draft (Task 5) is stored, because only a draft has no
//! source of truth to derive from.

pub mod render;
mod views;

use std::collections::BTreeMap;

use geode_core::config::{Config, Layer};

use crate::dialogmode::DialogMode;

/// Which config domain a dialog is browsing. One variant per adapter
/// module under this directory; `Views` is the only one built so far
/// (spec §8 has the other four, each arriving with its own adapter).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Domain {
    Views,
}

/// Which stage of the scaffold is on screen.
///
/// `Edit` exists now, unconstructed, because it is what makes the
/// `escape` ladder's [`crate::dialogmode::EscapeStep::PreviousStage`]
/// rung meaningful here — see [`ObjectDialogState::has_previous_stage`],
/// which is written against this enum rather than against a literal
/// `false` precisely so Task 5 turns the rung on by constructing the
/// variant rather than by reshaping the call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Stage {
    Browse,
    Edit { object: String },
}

/// One named object as the browse list shows it.
///
/// `layer`, `overridden` and `drifted` are the three provenance markers
/// spec §5 defines; they exist because whole-object override is invisible
/// otherwise — a user who overrode a desk view sees their copy and no
/// hint that a desk version exists underneath it, nor that it has since
/// moved on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjectRow {
    pub name: String,
    /// A one-line description of the object, from its own adapter — the
    /// second, muted line of the row, and half of what the filter
    /// matches against.
    pub summary: String,
    /// The last layer whose doc defines this name: the one whose copy
    /// actually takes effect, since every doc these dialogs edit is
    /// atomic at depth 1 (`config::merge::atomic_depth`) and so is
    /// replaced whole rather than merged key-by-key.
    pub layer: Layer,
    /// The user layer defines this object **and so does an earlier
    /// layer**. Both halves matter: Task 5 offers `Revert to desk` on an
    /// overridden row, and revert deletes the user's copy — on a view
    /// only the user layer defines, that would delete the view outright
    /// rather than restore anything. See [`derive_rows`].
    pub overridden: bool,
    /// Always `false` today, and deliberately still a field.
    ///
    /// Drift (spec §5.2) is "the layer I overrode has changed since I
    /// overrode it", which cannot be computed from `Config` alone: an
    /// override freezes a copy, so the shadowed layer's text at override
    /// time has to have been recorded somewhere. That record is
    /// `overrides.toml`, Part 2's work. Inventing a stand-in here — say,
    /// comparing the user's copy against the desk's current one — would
    /// mark every deliberate customisation as drifted, which is exactly
    /// backwards, so the honest value until the sidecar exists is
    /// `false`.
    pub drifted: bool,
}

impl Domain {
    /// The config doc this domain's objects live in — the file stem, as
    /// `Config::layered_docs` keys them.
    pub fn doc(self) -> &'static str {
        match self {
            Domain::Views => views::DOC,
        }
    }

    /// The dialog's title, and the word the footer uses for one object.
    pub fn title(self) -> &'static str {
        match self {
            Domain::Views => "Views",
        }
    }

    /// Every named object in this domain, with its provenance markers.
    ///
    /// The walk itself is [`derive_rows`], shared by every domain: the
    /// `layer`/`overridden` derivation is the dangerous part (see
    /// [`ObjectRow::overridden`]) and belongs in one tested place, not
    /// copied into each adapter. An adapter supplies only the summary
    /// line, which is the only part that is actually domain-specific.
    pub fn objects(self, config: &Config) -> Vec<ObjectRow> {
        match self {
            Domain::Views => derive_rows(config, views::DOC, views::summary),
        }
    }
}

/// Every object named in `doc`'s layered documents, one row each, sorted
/// by name.
///
/// `Config::layered_docs` hands back the per-layer documents in Builtin →
/// Desk → User order, so a single ordered walk answers both markers
/// (spec §5.1 — "no new machinery"):
///
/// - `layer` is the **last** layer whose doc contains the name, which is
///   also the copy that takes effect: every doc these dialogs edit is
///   atomic at depth 1, so a later layer's table replaces the earlier
///   one whole;
/// - `overridden` is the user layer containing it **and** some earlier
///   layer containing it too. The second half is what stops Task 5
///   offering `Revert to desk` on a view no desk ever had — reverting
///   there would delete the user's own view rather than restore
///   anything.
///
/// Sorted by name rather than kept in file order: rows come from up to
/// three documents, so "file order" would mean one file's order followed
/// by whatever names the next file added, which is neither the user's
/// nor the desk's order and shifts as soon as anything is overridden.
/// Alphabetical is the one ordering that stays put.
///
/// `config_version` is skipped — it is the schema stamp every layered
/// doc carries, not an object.
fn derive_rows(config: &Config, doc: &str, summary: fn(&toml::Value) -> String) -> Vec<ObjectRow> {
    // Accumulated by name — one name can appear in up to three documents
    // and each appearance updates the same row — in a `BTreeMap`, whose
    // key order IS the by-name order described above, so the rows come
    // out sorted without a separate pass. The `Vec<Layer>` beside each
    // row is every layer that defined it, which is what the `overridden`
    // question below needs and the row itself does not carry.
    let mut rows: BTreeMap<String, (Vec<Layer>, ObjectRow)> = BTreeMap::new();
    for layered in config.layered_docs(doc) {
        for (name, value) in &layered.table {
            if name == "config_version" {
                continue;
            }
            let entry = rows.entry(name.clone()).or_insert_with(|| {
                (
                    Vec::new(),
                    ObjectRow {
                        name: name.clone(),
                        summary: String::new(),
                        layer: layered.layer,
                        overridden: false,
                        drifted: false,
                    },
                )
            });
            entry.0.push(layered.layer);
            // Last writer wins, which is the merge's own rule: both the
            // winning layer and the summary describe the copy that
            // actually takes effect.
            entry.1.layer = layered.layer;
            entry.1.summary = summary(value);
        }
    }
    rows.into_values()
        .map(|(layers, mut row)| {
            row.overridden =
                layers.contains(&Layer::User) && layers.iter().any(|l| *l < Layer::User);
            row
        })
        .collect()
}

/// One open object dialog's pure state — the analogue of
/// `keybindings_view::KeybindingsState`, and stored on `ShellView` the
/// same way, with its `ScrollHandle` in a sibling field rather than in
/// here (see this module's own "Layout" note).
#[derive(Debug)]
pub struct ObjectDialogState {
    pub domain: Domain,
    pub stage: Stage,
    /// Index into the **filtered** list ([`visible_rows`]), not the full
    /// one — the palette's convention, shared by every list surface in
    /// this crate and what `vimnav::apply` clamps against.
    pub selected: usize,
    /// The filter query, mirrored here from `ShellView::dialog_input` by
    /// that field's `InputEvent::Change` subscription. The `Input` owns
    /// the text; this is the pure copy the rows are ranked against. It
    /// survives leaving filter mode, because the ladder's first rung
    /// keeps the query applied: leaving a search leaves you on the
    /// match.
    pub query: String,
    /// `Normal` on open — bare letters are verbs, and the shared filter
    /// input is left blurred so they reach [`render::handle_key`] rather
    /// than being typed.
    pub mode: DialogMode,
    /// A one-line report about the keystroke just pressed, painted in the
    /// footer and dropped at the next keystroke or click. Same contract
    /// as `KeybindingsState::notice`: a verb that deliberately did
    /// nothing says so, because a key that appears inert is the defect
    /// class this interaction model exists to remove.
    pub notice: Option<String>,
}

impl ObjectDialogState {
    /// Hand-written rather than `Default`-derived for the reason
    /// `KeybindingsState`'s own `Default` is: the opening mode is a
    /// per-surface decision and deserves one explicit, greppable line
    /// (`DialogMode` has no `Default` on purpose).
    pub fn new(domain: Domain) -> Self {
        Self {
            domain,
            stage: Stage::Browse,
            selected: 0,
            query: String::new(),
            mode: DialogMode::Normal,
            notice: None,
        }
    }

    /// Replace the query — the pure half of the `InputEvent::Change`
    /// subscription. Resets the selection to the top match (after an
    /// edit the old index points at an unrelated row) and drops the
    /// notice, which named a row the re-ranked list has just moved the
    /// selection off.
    pub fn set_query(&mut self, query: String) {
        self.query = query;
        self.selected = 0;
        self.notice = None;
    }

    /// Whether `escape` has a stage to step back into before it closes
    /// the dialog — the third rung of
    /// [`crate::dialogmode::escape_step`]'s ladder, and this scaffold is
    /// its first consumer (the keybinding dialog is one flat list and
    /// always passes `false`).
    ///
    /// Today it is always `false`, because `Browse` is the only stage
    /// this task builds and nothing constructs [`Stage::Edit`]. It is
    /// written as a predicate over the stage anyway, rather than as a
    /// literal `false` at the call site: Task 5 adds the edit stage, and
    /// the rung must then turn on by itself. A `false` spelled at the
    /// call site is exactly the shape that gets left behind when the
    /// stage arrives, and the failure would be silent — `escape` in the
    /// edit stage would close the whole dialog instead of going back.
    pub fn has_previous_stage(&self) -> bool {
        matches!(self.stage, Stage::Edit { .. })
    }
}

/// The text one row exposes to the filter: its name and summary —
/// exactly what the row paints, and nothing more. `keybindings_view`'s
/// own `searchable_text` carries the same rule and the review finding
/// behind it: matching text the user cannot see breaks the agreement
/// between what ranked and what is highlighted.
pub fn searchable_text(row: &ObjectRow) -> String {
    format!("{} {}", row.name, row.summary)
}

/// The rows this dialog currently shows, ranked by
/// [`crate::listfilter::rank`] — derived fresh at every call site
/// (render, key handling, click resolution), never cached, for the same
/// reason [`Domain::objects`] is not.
pub fn visible_rows(
    state: &ObjectDialogState,
    rows: &[ObjectRow],
) -> Vec<crate::listfilter::Ranked> {
    let texts: Vec<String> = rows.iter().map(searchable_text).collect();
    crate::listfilter::rank(&texts, &state.query)
}

/// Where the row named `clicked` currently sits in the *filtered* list,
/// or `None` if the filter is hiding it. Click handlers stay keyed by
/// object name — identity, never position, so a row survives the list
/// being re-ranked under it — and this is the one place that identity is
/// turned back into the index [`ObjectDialogState::selected`] speaks.
pub fn filtered_position(
    visible: &[crate::listfilter::Ranked],
    rows: &[ObjectRow],
    clicked: &str,
) -> Option<usize> {
    visible
        .iter()
        .position(|m| rows.get(m.row).is_some_and(|r| r.name == clicked))
}

#[cfg(test)]
mod tests {
    use super::*;
    use geode_core::config::{Config, ConfigSources, Layer, LayerDoc};

    /// A `Config` assembled from literal per-layer documents, in the
    /// Builtin → Desk → User order `merge_docs` documents its callers
    /// must pass. Everything goes through `ConfigSources::builtin` —
    /// that field is a `Vec<LayerDoc>` whose entries carry their OWN
    /// `layer`, and `Config::load` pushes them into the layered map in
    /// slice order without re-stamping it, so this reproduces a
    /// three-layer config exactly while staying a pure unit test (the
    /// desk and user fields are directories, and reading them would put
    /// filesystem I/O in the pure core's own tests).
    fn config_from(docs: &[(Layer, &str, &str)]) -> Config {
        let layered: Vec<LayerDoc> = docs
            .iter()
            .map(|(layer, name, text)| LayerDoc {
                layer: *layer,
                name: (*name).to_string(),
                file: std::path::PathBuf::from(format!("<test:{}:{name}>", layer.name())),
                table: text.parse().expect("fixture TOML parses"),
            })
            .collect();
        Config::load(&ConfigSources {
            builtin: layered,
            desk: None,
            user: None,
        })
    }

    #[test]
    fn a_row_carries_the_layer_that_won_and_marks_a_user_override() {
        let config = config_from(&[
            (
                Layer::Builtin,
                "views",
                "[tree]\ndataset = \"risk\"\n[wide]\ndataset = \"risk\"\n",
            ),
            (Layer::User, "views", "[tree]\ndataset = \"risk\"\n"),
        ]);
        let rows = Domain::Views.objects(&config);
        let tree = rows.iter().find(|r| r.name == "tree").expect("tree");
        let wide = rows.iter().find(|r| r.name == "wide").expect("wide");
        assert_eq!(tree.layer, Layer::User);
        assert!(
            tree.overridden,
            "user layer plus an earlier layer means overridden"
        );
        assert_eq!(wide.layer, Layer::Builtin);
        assert!(
            !wide.overridden,
            "a view only one layer defines is not overridden"
        );
    }

    /// A view the user layer alone defines is theirs, not an override —
    /// getting this wrong would offer "Revert to desk" on a view no desk
    /// has, and reverting would delete it.
    #[test]
    fn a_user_only_object_is_not_marked_overridden() {
        let config = config_from(&[(Layer::User, "views", "[mine]\ndataset = \"risk\"\n")]);
        let rows = Domain::Views.objects(&config);
        assert_eq!(rows.len(), 1);
        assert!(!rows[0].overridden);
    }

    /// The middle layer is the case a two-layer fixture cannot see: a
    /// desk view with no user copy is the desk's, not overridden, and a
    /// desk view the user overrode reports the USER as the winning layer
    /// even though a builtin copy also exists underneath.
    #[test]
    fn the_desk_layer_wins_over_builtin_and_loses_to_the_user() {
        let config = config_from(&[
            (Layer::Builtin, "views", "[tree]\ndataset = \"a\"\n"),
            (
                Layer::Desk,
                "views",
                "[tree]\ndataset = \"b\"\n[desk_only]\ndataset = \"c\"\n",
            ),
            (Layer::User, "views", "[tree]\ndataset = \"d\"\n"),
        ]);
        let rows = Domain::Views.objects(&config);
        let tree = rows.iter().find(|r| r.name == "tree").expect("tree");
        assert_eq!(tree.layer, Layer::User);
        assert!(tree.overridden);
        assert!(
            tree.summary.contains('d'),
            "the summary must describe the copy that takes effect, got {:?}",
            tree.summary
        );
        let desk_only = rows.iter().find(|r| r.name == "desk_only").expect("desk");
        assert_eq!(desk_only.layer, Layer::Desk);
        assert!(
            !desk_only.overridden,
            "a desk view with no user copy is not overridden — reverting it \
             would delete the desk's own view"
        );
    }

    /// Drift needs `overrides.toml` (spec §5.2, Part 2). Until then the
    /// honest answer is `false` for every row, including an overridden
    /// one — a stand-in derived from the current config would mark every
    /// deliberate customisation as drifted.
    #[test]
    fn drift_is_not_claimed_before_overrides_toml_exists() {
        let config = config_from(&[
            (Layer::Desk, "views", "[tree]\ndataset = \"a\"\n"),
            (Layer::User, "views", "[tree]\ndataset = \"b\"\n"),
        ]);
        let rows = Domain::Views.objects(&config);
        assert!(rows[0].overridden, "sanity: this row IS an override");
        assert!(!rows[0].drifted);
    }

    /// `config_version` is a schema stamp every layered doc carries, not
    /// an object — a row for it would be a view the user could try to
    /// open and edit.
    #[test]
    fn the_config_version_stamp_is_not_an_object() {
        let config = config_from(&[(
            Layer::User,
            "views",
            "config_version = 1\n[tree]\ndataset = \"risk\"\n",
        )]);
        let names: Vec<String> = Domain::Views
            .objects(&config)
            .into_iter()
            .map(|r| r.name)
            .collect();
        assert_eq!(names, vec!["tree".to_string()]);
    }

    /// A domain with no doc at all — a fresh install with no views —
    /// browses as an empty list rather than failing.
    #[test]
    fn a_missing_doc_is_an_empty_list() {
        let config = config_from(&[]);
        assert!(Domain::Views.objects(&config).is_empty());
    }

    /// The rung Task 5 turns on. `Browse` has nothing behind it, so
    /// `escape` must reach the close rung; `Edit` does, and the ladder
    /// must stop there first.
    #[test]
    fn only_a_nested_stage_offers_escape_a_previous_stage() {
        let mut state = ObjectDialogState::new(Domain::Views);
        assert_eq!(
            state.mode,
            DialogMode::Normal,
            "dialogs open in normal mode"
        );
        assert!(!state.has_previous_stage());
        state.stage = Stage::Edit {
            object: "tree".to_string(),
        };
        assert!(state.has_previous_stage());
    }

    /// Editing the query re-ranks the list, so the old index points at an
    /// unrelated row and any notice names a row the selection has just
    /// left.
    #[test]
    fn setting_the_query_resets_the_selection_and_drops_the_notice() {
        let mut state = ObjectDialogState::new(Domain::Views);
        state.selected = 4;
        state.notice = Some("nothing to do".to_string());
        state.set_query("tr".to_string());
        assert_eq!(state.query, "tr");
        assert_eq!(state.selected, 0);
        assert!(state.notice.is_none());
    }

    /// The filter sees the name and the summary — what the row paints —
    /// so a match can always be explained by a highlight.
    #[test]
    fn the_filter_matches_the_two_lines_the_row_actually_shows() {
        let config = config_from(&[(
            Layer::User,
            "views",
            "[tree]\ndataset = \"risk_snapshot\"\ncolumns = [{ name = \"npv\" }]\n",
        )]);
        let rows = Domain::Views.objects(&config);
        let text = searchable_text(&rows[0]);
        assert!(text.contains("tree"), "{text}");
        assert!(
            text.contains("risk_snapshot"),
            "the dataset is on the row, so it must be searchable: {text}"
        );
    }

    /// Rows are ordered by name, not by the order three separate files
    /// happen to list them in.
    #[test]
    fn rows_are_ordered_by_name_across_layers() {
        let config = config_from(&[
            (Layer::Builtin, "views", "[zebra]\ndataset = \"a\"\n"),
            (Layer::User, "views", "[alpha]\ndataset = \"a\"\n"),
        ]);
        let names: Vec<String> = Domain::Views
            .objects(&config)
            .into_iter()
            .map(|r| r.name)
            .collect();
        assert_eq!(names, vec!["alpha".to_string(), "zebra".to_string()]);
    }
}
