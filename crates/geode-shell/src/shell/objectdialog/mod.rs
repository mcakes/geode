//! The config dialogs' shared scaffold: browse a config domain's named
//! objects, and edit one
//! (`docs/superpowers/specs/2026-09-08-geode-phase-4c-config-dialogs-design.md`).
//!
//! Phase 4c replaced an earlier design that would have put a TOML text
//! editor in a tile. Instead every config domain — views, groupings,
//! scopes, sources, the read-only schema — gets a purpose-built dialog
//! on this one scaffold, so a trader edits *objects with fields* rather
//! than text, and so the layer a change lands in is a property of the
//! scaffold rather than a thing each dialog remembers to get right.
//! Both stages were first built over one adapter, [`Domain::Views`] — the
//! hardest shape first, deliberately (spec §14), so the vocabulary was
//! settled before the thinner adapters depended on it. Views is the only
//! domain whose fields split across two destinations, and
//! [`Destination`] is what makes that split mechanical rather than a
//! special case inside one adapter.
//!
//! [`Domain::Groupings`] is the first of those thinner adapters, and it
//! needed one more thing from the scaffold that Views alone never
//! exercised: its object's own value is a bare array (`3 = ["book",
//! "lhu"]`), not a table, so [`Domain::to_table`] renders a
//! `toml_edit::Item` rather than a `toml_edit::Table` — see
//! `groupings.rs`'s module doc for the full story.
//!
//! ## Layout
//!
//! Split the way `keybindings_view` and `picker` are, one directory
//! further out because a domain adapter is a file of its own (spec §4):
//!
//! - this module — the pure core: [`Stage`], [`ObjectDialogState`],
//!   [`Draft`] and its field vocabulary ([`Field`], [`FieldKind`],
//!   [`ListItem`], [`Destination`]), [`ObjectRow`], [`Domain`], and the
//!   one derivation of `layer` and `overridden` every domain shares;
//! - [`views`] — the `Domain::Views` adapter: the doc it reads, the
//!   one-line summary a view row shows, the fields a view has and which
//!   file each one is written to, and nothing else;
//! - [`groupings`] — the `Domain::Groupings` adapter: the nine grouping
//!   slots, one dimension chain each;
//! - [`render`] — the gpui shell: `open`, the [`dialog::ModalKeyHandler`]
//!   both stages come through, and the painted list, fields and action
//!   bar.
//!
//! No `gpui` type appears in this file, so every transition and every
//! marker below is unit-testable without a window — the same split, for
//! the same reason, that keeps `KeybindingsState` free of the scroll
//! handle that sits beside it on `ShellView`.
//!
//! ## Browse rows are derived; the edit stage paints its draft
//!
//! [`Domain::objects`] runs fresh on every render and every keystroke.
//! That is the contract `keybindings_view::derive_rows` and
//! `settings_view::derive_rows` both hold, and the reason those dialogs
//! cannot show a stale value: a config reload lands in
//! `ShellView::services.config` with no notification to any dialog, so
//! anything cached here would be wrong from the next 500 ms watcher tick
//! onward.
//!
//! The **edit stage** is the deliberate exception: its rows come from the
//! stored [`Draft`], not from `Config`. That is not a cache, and it is
//! what makes a config edit instant. A keystroke records its change on a
//! pending batch that is merged and applied 250 ms later ([`apply`]), so
//! `services.config` is knowingly up to one debounce behind — a row
//! derived from it would show the trader their own keystroke a quarter of
//! a second late, which is the lag this whole design exists to remove.
//! The draft is the same value the flush is about to merge, rendered
//! through the same [`Domain::to_table`], and a failed write rebuilds it
//! from the reverted config, so the two cannot drift apart.

pub mod apply;
mod groupings;
pub mod render;
mod scopes;
mod views;

use std::collections::{BTreeMap, BTreeSet};

use geode_core::config::{Config, Diagnostic, Layer};

use crate::dialogmode::DialogMode;

/// Which config domain a dialog is browsing. One variant per adapter
/// module under this directory (spec §8 has two more — Sources and the
/// read-only Schema — still to arrive with their own adapters).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Domain {
    Views,
    Groupings,
    Scopes,
}

/// Which stage of the scaffold is on screen.
///
/// `Edit` is what makes the `escape` ladder's
/// [`crate::dialogmode::EscapeStep::PreviousStage`] rung reachable — this
/// dialog is the design's first consumer of that rung, and
/// [`ObjectDialogState::has_previous_stage`] is written against this enum
/// rather than against a literal `false` so constructing the variant is
/// all it took to turn the rung on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Stage {
    Browse,
    /// The browse list with the filter row replaced by a *name* field
    /// (§18.2): `n` enters it, `enter` creates, `escape` returns to
    /// `Browse` with nothing written. A stage rather than a flag on
    /// `Browse` so `has_previous_stage` turns the escape ladder's third
    /// rung on by construction, the same way `Edit` did.
    Naming,
    /// Editing one object's fields. The draft itself lives in
    /// [`ObjectDialogState::draft`] rather than in here, because the
    /// *name* is what identifies the stage (it is what `escape` restores
    /// the browse selection to) while the draft is mutable state that a
    /// `PartialEq` stage comparison has no business walking.
    Edit {
        object: String,
    },
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
    /// The layer whose copy takes effect, or `None` when no layer defines
    /// the object at all and it is on the list only because the domain's
    /// roster names it (`Domain::roster` — a grouping slot nothing has
    /// filled). `Option` rather than a `configured: bool` beside a
    /// placeholder `Layer`: every reader of this field decides something
    /// destructive or forking on it (`arm_delete`, `would_fork`), and a
    /// placeholder value would answer those questions with a lie.
    pub layer: Option<Layer>,
    /// The user layer defines this object **and so does an earlier
    /// layer**. Both halves matter: the edit stage offers `Revert to
    /// desk` on an overridden row, and revert deletes the user's copy —
    /// on a view only the user layer defines, that would delete the view
    /// outright rather than restore anything. See [`derive_rows`].
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
    /// `Config::layered_docs` keys them. The one spelling of that fact:
    /// [`objects`](Self::objects) reads the doc name from here rather
    /// than naming the adapter's own constant a second time, so the two
    /// cannot drift as adapters are added.
    pub fn doc(self) -> &'static str {
        match self {
            Domain::Views => views::DOC,
            Domain::Groupings => groupings::DOC,
            Domain::Scopes => scopes::DOC,
        }
    }

    /// The dialog's title, and the word the footer uses for one object.
    pub fn title(self) -> &'static str {
        match self {
            Domain::Views => "Views",
            Domain::Groupings => "Groupings",
            Domain::Scopes => "Scopes",
        }
    }

    /// The plural noun the browse crumb counts (`2 views`, `9 slots`,
    /// `3 saved`) — not `title()`, whose "Groupings" would count the
    /// wrong thing.
    pub fn crumb_noun(self) -> &'static str {
        match self {
            Domain::Views => "views",
            Domain::Groupings => "slots",
            Domain::Scopes => "saved",
        }
    }

    /// How this domain describes one object on its browse row — the only
    /// genuinely domain-specific part of a row, and therefore the only
    /// part an adapter gets to supply.
    fn summary_fn(self) -> fn(&toml::Value) -> String {
        match self {
            Domain::Views => views::summary,
            Domain::Groupings => groupings::summary,
            Domain::Scopes => scopes::summary,
        }
    }

    /// The presentation doc this domain's objects can be personalised
    /// through without forking (spec §4.1), if it has one at all.
    ///
    /// `None` for a domain none of whose fields carry
    /// [`Destination::Presentation`] — Groupings, whose `dimensions` is
    /// entirely [`Destination::Doc`] (`groupings.rs`'s module doc has the
    /// reasoning) — so [`derive_rows`]'s "personalised without
    /// overriding" check, and `render::run_confirmed`'s removal list,
    /// both have either a real doc name to look for or nothing to look
    /// for, rather than a name that could never correspond to a file.
    fn presentation_doc(self) -> Option<&'static str> {
        match self {
            Domain::Views => Some(views::PRESENTATION_DOC),
            // Scopes has no presentation doc for the same reason
            // Groupings does not: every field this domain has is
            // `Destination::Doc` (`scopes.rs`'s module doc).
            Domain::Groupings | Domain::Scopes => None,
        }
    }

    /// The names a domain's browse list always shows, configured or not.
    /// `Some` only for Groupings (§18.4): the nine `ctrl+1..9` slots are
    /// a fixed keyboard, so an unfilled slot is a row that reads `empty`
    /// rather than a row that does not exist — and there is no `n`,
    /// because nothing can be created that is not already on the list.
    /// Slot `0` is not here: `ctrl+0` is `frame::slot_clear`, the view's
    /// own grouping, and `GroupingSlots` is nine wide (user ruling
    /// 2026-09-10).
    pub(super) fn roster(self) -> Option<&'static [&'static str]> {
        match self {
            Domain::Groupings => Some(&["1", "2", "3", "4", "5", "6", "7", "8", "9"]),
            Domain::Views | Domain::Scopes => None,
        }
    }

    /// Every named object in this domain, with its provenance markers.
    ///
    /// **Deliberately not a `match`.** The `layer`/`overridden`
    /// derivation is the dangerous computation on this whole surface
    /// (see [`ObjectRow::overridden`]: a wrong `overridden` makes the
    /// edit stage offer a destructive `Revert to desk`), so every domain
    /// must share the one tested walk. A per-domain `match` here would
    /// merely *discourage* an adapter from doing its own walk and
    /// diverging; an unconditional call makes that unrepresentable — the
    /// only things a domain decides are its doc name, its summary line
    /// and (optionally) its presentation doc, and all three arrive
    /// through the small matches above. Part 2 adds two more adapters
    /// onto this exact seam, which is why the hole is closed while there
    /// are still only two.
    pub fn objects(self, config: &Config) -> Vec<ObjectRow> {
        derive_rows(
            config,
            self.doc(),
            self.presentation_doc(),
            self.roster(),
            self.summary_fn(),
        )
    }

    /// Is `name` already spoken for in this domain (spec §18.2's "a name
    /// any layer already holds is refused")?
    ///
    /// **Wider than [`Domain::objects`] on purpose, and that is the
    /// whole reason it exists.** The rows are what the browse list can
    /// show — `doc`'s layered keys plus the roster — but a name can also
    /// be held by the user's presentation overlay alone
    /// ([`personalised_names`]): the desk dropped a view the trader had
    /// hidden a column on, so `view_presentation.toml` still names it
    /// while no layer of `views.toml` does. Creating over that name made
    /// a fresh user view that silently inherited the orphaned overlay's
    /// `hidden`/`order`/`width`, and — being user-only — `r` then
    /// refused it, so no dialog verb could clear it. A row-based check
    /// cannot see that name at all, which is why the refusal reads this
    /// union rather than the list.
    ///
    /// `config_version` never reaches here: `check_object_name` refuses
    /// it before the caller asks.
    pub fn name_taken(self, config: &Config, name: &str) -> bool {
        if self.roster().is_some_and(|roster| roster.contains(&name)) {
            return true;
        }
        if config
            .layered_docs(self.doc())
            .iter()
            .any(|layered| layered.table.contains_key(name))
        {
            return true;
        }
        personalised_names(config, self.presentation_doc()).contains(name)
    }
}

/// The objects a domain's user-layer presentation overlay names — the
/// set [`derive_rows`] reads as "personalised without overriding" and
/// [`Domain::name_taken`] reads as "a name that is spoken for even
/// though nothing lists it". One walk, shared, because the two answers
/// have to agree: a name this set holds and `doc` does not is exactly
/// the case that produces no browse row at all, so nothing on screen
/// could reveal the two walks having drifted apart.
///
/// Empty outright for a domain with no presentation doc
/// (`presentation_doc: None`) — there is nothing to be personalised
/// through. `config_version` is the schema stamp every layered doc
/// carries, not an object.
fn personalised_names<'a>(config: &'a Config, presentation_doc: Option<&str>) -> BTreeSet<&'a str> {
    presentation_doc
        .map(|presentation_doc| {
            config
                .layered_docs(presentation_doc)
                .iter()
                .filter(|layered| layered.layer == Layer::User)
                .flat_map(|layered| layered.table.keys())
                .filter(|name| *name != "config_version")
                .map(String::as_str)
                .collect()
        })
        .unwrap_or_default()
}

/// Every object named in `doc`'s layered documents, plus every name
/// `roster` fixes as always-listed (§18.4 — Groupings' nine slots), one
/// row each, sorted by name.
///
/// A rostered name no doc defines gets a row with `layer: None` and
/// `summary: "empty"` — seeded before the layered walk below so a name
/// the walk does touch overwrites that placeholder in place, and a name
/// it never touches is left exactly as seeded. `roster: None` (every
/// domain but Groupings) seeds nothing, so those domains list only what
/// their docs actually define, as before.
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
///   layer containing it too. The second half is what stops the edit
///   stage offering `Revert to desk` on a view no desk ever had —
///   reverting there would delete the user's own view rather than
///   restore anything (`render::arm_revert` gates on exactly this).
///
/// "The user layer containing it" spans `presentation_doc` as well as
/// `doc`, when the domain has one, and that is not a refinement: §4.1's
/// whole design is that hiding a column writes `view_presentation.toml`
/// and forks nothing, so the commonest user override there leaves
/// `doc`'s user layer empty. Reading the markers off `doc` alone
/// answered `r` with "no user override to revert" while the file `r`
/// would have removed sat on disk. The `some earlier layer` half is
/// still read off `doc` only — that is the half that guarantees
/// reverting leaves an object behind. A domain with no presentation doc
/// at all (`presentation_doc: None`) simply has nothing this half can
/// add.
///
/// Sorted by name rather than kept in file order: rows come from up to
/// three documents, so "file order" would mean one file's order followed
/// by whatever names the next file added, which is neither the user's
/// nor the desk's order and shifts as soon as anything is overridden.
/// Alphabetical is the one ordering that stays put.
///
/// `config_version` is skipped — it is the schema stamp every layered
/// doc carries, not an object.
fn derive_rows(
    config: &Config,
    doc: &str,
    presentation_doc: Option<&str>,
    roster: Option<&'static [&'static str]>,
    summary: fn(&toml::Value) -> String,
) -> Vec<ObjectRow> {
    // Objects the user layer has personalised without overriding: a
    // `view_presentation.toml` table names the object and forks nothing.
    let personalised = personalised_names(config, presentation_doc);
    // Accumulated by name — one name can appear in up to three documents
    // and each appearance updates the same row — in a `BTreeMap`, whose
    // key order IS the by-name order described above, so the rows come
    // out sorted without a separate pass. The `Vec<Layer>` beside each
    // row is every layer that defined it, which is what the `overridden`
    // question below needs and the row itself does not carry.
    let mut rows: BTreeMap<String, (Vec<Layer>, ObjectRow)> = BTreeMap::new();
    // Seeded before the layered walk, not after: a name the walk touches
    // has to update this entry in place (`entry.1.layer = Some(..)`
    // below), and a name it never touches has to survive untouched —
    // `empty`, `layer: None`. Seeding afterward would need its own
    // "don't overwrite what the walk already set" check; seeding first
    // makes the walk's own `or_insert_with`/overwrite the only rule.
    for name in roster.unwrap_or(&[]) {
        rows.insert(
            (*name).to_string(),
            (
                Vec::new(),
                ObjectRow {
                    name: (*name).to_string(),
                    summary: "empty".to_string(),
                    layer: None,
                    overridden: false,
                    drifted: false,
                },
            ),
        );
    }
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
                        layer: Some(layered.layer),
                        overridden: false,
                        drifted: false,
                    },
                )
            });
            entry.0.push(layered.layer);
            // Last writer wins, which is the merge's own rule: both the
            // winning layer and the summary describe the copy that
            // actually takes effect.
            entry.1.layer = Some(layered.layer);
            entry.1.summary = summary(value);
        }
    }
    rows.into_values()
        .map(|(layers, mut row)| {
            let mine = layers.contains(&Layer::User) || personalised.contains(row.name.as_str());
            row.overridden = mine && layers.iter().any(|l| *l < Layer::User);
            row
        })
        .collect()
}

// ---------------------------------------------------------------------
// The edit stage: fields, destinations, and the draft
// ---------------------------------------------------------------------

/// Which user-layer document one field's value is written to (spec §4.1).
///
/// **This enum is the whole reason the Views dialog is safe to use.**
/// Dragging a column's width is the commonest edit a trader makes, and
/// writing it into `views.toml` would fork the desk's view — a forked
/// view is frozen, so when the desk adds a column next week the trader
/// never sees it. Order, inclusion and width therefore carry
/// [`Destination::Presentation`] and land in `view_presentation.toml`,
/// which `config::load_views` merges *over* the view; only a
/// definitional change (the dataset, the column *set*) carries
/// [`Destination::Doc`] and forks anything.
///
/// It is an enum on the field rather than a rule inside the adapter so
/// the split is mechanical: [`Draft::writes_by_destination`] groups by
/// it, and the flush makes one `config_write::edit` call per group
/// without knowing what either file is for. Exactly two places in the
/// scaffold know `view_presentation.toml` exists — [`Destination::doc`]
/// and [`Domain::presentation_doc`], the same answer asked two ways
/// ("which file does this destination write" and "does this domain have
/// an overlay file at all") — and both route to the Views adapter's own
/// `PRESENTATION_DOC` constant rather than spelling the name again.
///
/// `Ord` because the groups are collected into a `BTreeMap`, so a flush
/// writes its files in a fixed order rather than a hash-random one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Destination {
    /// The domain's own doc, user layer.
    Doc,
    /// `view_presentation.toml`, user layer.
    Presentation,
}

impl Destination {
    /// The config doc (file stem) this destination writes, for `domain`.
    pub fn doc(self, domain: Domain) -> &'static str {
        match (self, domain) {
            (Destination::Doc, domain) => domain.doc(),
            (Destination::Presentation, Domain::Views) => views::PRESENTATION_DOC,
            // Every Groupings field is `Destination::Doc` (spec §8.2 —
            // there is nothing presentational about a dimension chain),
            // so this arm exists only to keep the match exhaustive as
            // domains are added, not because anything can reach it.
            (Destination::Presentation, Domain::Groupings) => {
                unreachable!("Groupings has no Presentation-destined fields")
            }
            // Every Scopes field is `Destination::Doc` too (`scopes.rs`'s
            // module doc: the whole object is a read-only summary), so
            // this arm exists only to keep the match exhaustive.
            (Destination::Presentation, Domain::Scopes) => {
                unreachable!("Scopes has no Presentation-destined fields")
            }
        }
    }
}

/// One entry of an [`FieldKind::OrderedList`].
///
/// A deliberately fixed, bounded shape rather than general nesting (spec
/// §3.1): a view's columns and a grouping's dimensions are the only
/// ordered lists in the config model and both fit it, while arbitrary
/// sub-fields would make the edit stage recursive for no reader.
///
/// **Membership is not a field here** (§18.7): which of a field's two
/// lists an item sits in IS its membership, so the same `ListItem` shape
/// describes one of the object's own entries and one of the catalogue
/// entries it may gain. Where the two differ is what
/// [`FieldKind::OrderedList`] documents, and nothing has to keep a flag
/// and a position agreeing with each other.
///
/// `width` is `Option<f32>` and not the spec sketch's `u32` because
/// `ColumnPresentation::width` — the thing it round-trips through — is
/// `Option<f32>`. `None` means "no width declared", which is not the
/// same as zero: a declared zero would be a column of no width.
#[derive(Debug, Clone, PartialEq)]
pub struct ListItem {
    pub name: String,
    /// Shown rather than hidden. For a view this is the inverse of
    /// `ColumnPresentation::hidden`, and it is presentation, never the
    /// column set: a hidden column stays in `ViewSpec::columns`, so the
    /// compiler still selects it and unhiding costs nothing.
    pub included: bool,
    pub width: Option<f32>,
    /// The `[[columns]]` `kind` a first-time write of this item needs —
    /// `"dimension"` or `"measure"` for Views (`views::fields`'s own doc
    /// has the schema-role mapping and why a `key`/`attribute` column
    /// never reaches this list at all), the ViewColumn variant's own kind
    /// for a column already in the view. `None` where a list has no
    /// column-kind concept (Groupings' `dimensions`): read only by
    /// `views::columns_for`, on the one path that writes a brand new
    /// `[[columns]]` entry — a column already in `Draft::source` keeps
    /// its own table, `kind` included, untouched by this field entirely.
    pub kind: Option<String>,
}

/// The closed vocabulary an object's fields are built from (spec §3.1).
///
/// Closed on purpose: validation is then by construction — a `Choice`
/// offers only datasets that exist, an `OrderedList` only columns the
/// view has — which is what makes most of the loader's old diagnostics
/// unreachable from these dialogs.
///
/// Two variants have no key in this task and say so rather than
/// pretending: [`FieldKind::Text`] needs in-place editing behind `i`,
/// and [`FieldKind::MultiChoice`] needs a per-option row for `space` to
/// tick. Views uses neither; both arrive with the Part 2 adapter that
/// first needs one, which is also when a test can see them.
#[derive(Debug, Clone, PartialEq)]
pub enum FieldKind {
    Text(String),
    Number {
        value: i64,
        min: i64,
        max: i64,
    },
    Bool(bool),
    Choice {
        options: Vec<String>,
        selected: usize,
    },
    MultiChoice {
        options: Vec<String>,
        ticked: BTreeSet<String>,
    },
    /// The object's own ordered list, and — for a list a trader can add
    /// to — the catalogue of what may join it (spec §18.7). `items` is
    /// the only list that is ordered, written, counted or reorderable.
    ///
    /// `available` is `None` where ticking IS membership and there is no
    /// catalogue to promote out of (Groupings' `dimensions`, whose
    /// unticked rows are already in `items`), and `Some` — possibly
    /// EMPTY — where one exists (Views' `columns`, whose catalogue empties
    /// out once the trader has added every column the dataset offers).
    /// The distinction is load-bearing rather than tidy: `x` demotes into
    /// a catalogue that exists and refuses where none does, so reading
    /// "is this list catalogue-less" off `available.is_empty()` would make
    /// `x` go dead on a fully-added Views list — the exact regression
    /// `remove_selected`'s own doc records. `available` is unordered by
    /// construction: nothing writes it and nothing reads its order.
    ///
    /// Two lists rather than one list and a flag, so the
    /// members-before-available rule four mutators used to maintain by
    /// hand is not a rule at all.
    OrderedList {
        items: Vec<ListItem>,
        available: Option<Vec<ListItem>>,
    },
}

/// One editable property of one object.
#[derive(Debug, Clone, PartialEq)]
pub struct Field {
    /// The TOML key within the object (`dataset`, `columns`). Also what a
    /// diagnostic's key path is matched against once readers carry one
    /// (spec §8.5 — see [`Draft::diagnostics`]).
    pub key: String,
    pub label: String,
    pub kind: FieldKind,
    pub dest: Destination,
}

/// One row of the edit stage: a field, or one item of a field's ordered
/// list.
///
/// Flattened rather than nested so the cursor, `space` and `shift+j` all
/// speak one index — the same shape the browse list has, and the reason
/// `vimnav::apply` works unchanged in both stages.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditRow {
    Field(usize),
    /// One of the object's own list entries — `field`'s `items`.
    Item {
        field: usize,
        item: usize,
    },
    /// One row of a list's `available` catalogue. A variant of its own so
    /// a consumer cannot treat an available row as one of the object's
    /// own by omission — every match on [`EditRow`] has to say what
    /// `space`, `x`, a click or a label means here.
    Available {
        field: usize,
        item: usize,
    },
}

/// What a keystroke is waiting to have confirmed. Each of the three is
/// unrecoverable in its own direction — deleting the user's copy of an
/// object, throwing away a personal override, or forking an object out of
/// the layer that maintains it — so each takes a second, deliberate
/// keystroke rather than happening under one letter.
///
/// There is no `Discard`. It existed to guard the staged-draft model's
/// unsaved work; now every field edit is recorded the moment it is made,
/// on a batch that outlives the stage and the dialog and reaches disk on
/// its own timer, so leaving abandons nothing and a confirm there would
/// be a question about a state that cannot arise.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Confirm {
    Delete,
    Revert,
    /// A definitional change to an object the user's layer does not own.
    /// Applying it copies the object into the user layer — which
    /// **freezes** it: the desk's later changes stop reaching this trader
    /// (spec §4.1). The one edit in this dialog that asks before acting,
    /// and the reason it asks is that the cost lands weeks later.
    Fork,
    /// `o` on a saved scope (`Domain::Scopes` only): overwrite its
    /// contents with whatever the frame currently holds.
    ///
    /// `forks` is decided at arm time (`render::arm_overwrite`), from
    /// whether the scope under the cursor is the user layer's own —
    /// never from destructiveness alone. When it is not, the write does
    /// two things at once: it replaces the scope's content, and it
    /// forks it into the user layer, which **freezes** the desk's copy
    /// out the same way `Fork` does (spec §4.1) — invisible until weeks
    /// later if the prompt does not say so (spec §16). One confirm
    /// either way, not two in sequence (§16 keeps every confirm to a
    /// single row that never changes length): the payload is what lets
    /// one prompt disclose whichever consequence is real for *this*
    /// object, since "saved selection is lost" is true only when there
    /// is no desk copy underneath to fall back to.
    Overwrite {
        forks: bool,
    },
}

impl Confirm {
    /// The prompt, naming the object and the consequence rather than
    /// asking "are you sure": the interaction model's own copy rule, and
    /// gpui-component's design guide's.
    pub fn prompt(self, name: &str) -> String {
        match self {
            Confirm::Delete => format!("Delete '{name}' from your config?"),
            Confirm::Revert => format!("Throw away your changes to '{name}'?"),
            Confirm::Fork => {
                format!("Copy '{name}' to your config?")
            }
            // User-owned: nothing underneath to fall back to, so the
            // scope's previous contents really are gone.
            Confirm::Overwrite { forks: false } => format!(
                "Replace '{name}' with the frame's current scope? Its saved selection is lost."
            ),
            // Not user-owned: nothing is lost — the desk's own copy is
            // still there — but the write forks this one into the user
            // layer, exactly like `Fork`'s own consequence, and `r`
            // reverts it same as any other fork.
            Confirm::Overwrite { forks: true } => {
                format!("Replace '{name}' with the frame's current scope? 'r' reverts.")
            }
        }
    }
}

/// One object being edited.
///
/// The edit **buffer**, and what the edit stage actually paints: a
/// keystroke changes a field here — so the trader sees it at once — and
/// [`apply::commit_edit`] puts that change on the pending batch the next
/// flush merges, applies and writes (spec §7.1). The buffer also carries
/// the cursor, the confirm and the row structure, and it is what the
/// difference against [`Draft::baseline`] is taken from — that difference
/// is precisely "what this keystroke changed", which is what decides the
/// files a flush touches.
///
/// An earlier build staged here and wrote only on `s`, reasoning that a
/// write per keystroke would fire the 500 ms watcher mid-edit and reload
/// a half-finished object. What actually removes that hazard is that the
/// change never travels through the disk to reach the screen: the flush
/// merges the documents already in hand, and the watcher's reload of our
/// own write is a no-op (see [`apply`]).
#[derive(Debug, Clone)]
pub struct Draft {
    pub name: String,
    /// Created by `n` this session and not yet on the browse list the
    /// config derives — the edit header says `new` beside the layer until
    /// the dialog is left. Nothing else reads it: the write path treats a
    /// new object like any other Doc write.
    pub is_new: bool,
    pub fields: Vec<Field>,
    /// The object exactly as the merged doc holds it, kept so a
    /// [`Destination::Doc`] write can preserve everything the field
    /// vocabulary does not model — a view's `grouping`, `sort`, `joins`,
    /// per-column `format`, `label` and a derived column's `sql`.
    /// Rendering a Doc override from the fields alone would silently
    /// delete all of it.
    pub source: toml::Table,
    /// The fields as they were when the draft was built, or as the last
    /// applied edit left them. Which files an edit touches is this
    /// comparison and nothing else, so a keystroke that puts a value back
    /// where it started changes nothing and writes nothing — and a
    /// declined [`Confirm::Fork`] restores from here
    /// ([`Draft::revert_to_baseline`]).
    baseline: Vec<Field>,
    /// `source` as it stood at the same moment `baseline` did. For every
    /// domain but Scopes this never diverges from `source` after
    /// construction — nothing else in this scaffold mutates `source`
    /// directly — so it costs those domains nothing. Scopes' `o`
    /// (`scopes::overwrite_with`) is the one verb that replaces `source`
    /// wholesale while leaving the *painted* fields free to describe it
    /// however a summary function likes; comparing only `fields` against
    /// `baseline` would then make dirtiness depend on two summary
    /// strings never colliding, which is not a property `selects_summary`
    /// promises to keep as it grows. Comparing `source` directly closes
    /// that by construction: the actual object decides whether anything
    /// changed, not its rendering.
    baseline_source: toml::Table,
    /// Cursor over [`Draft::visible_rows`] (§18.3) — the FILTERED list,
    /// not [`Draft::rows`] — the same convention
    /// `ObjectDialogState::selected` holds for the browse stage. One
    /// cursor per stage, never both live at once.
    pub selected: usize,
    /// The edit stage's filter (§18.3), mirrored from the shared `Input`
    /// by `ObjectDialogState::set_query` exactly as the browse query is.
    /// Lives on the draft rather than beside it because `selected`
    /// indexes the FILTERED list and both must move together.
    pub query: String,
    /// [`Domain::validate`]'s output for the draft as it stands, refreshed
    /// on every change (spec §7.2).
    ///
    /// Shown against the object as a whole rather than against individual
    /// field rows: attaching a diagnostic to the field whose `key` matches
    /// its `path` needs `Diagnostic::path` (spec §8.5), which no reader
    /// carries yet — adding it means a new field on `Diagnostic` and a
    /// change to all 27 of its construction sites plus every reader that
    /// would fill it, none of which are files this task owns. Reported as
    /// a deviation rather than faked by matching on message text.
    pub diagnostics: Vec<Diagnostic>,
    /// The destructive keystroke waiting on a second one, if any. It
    /// replaces the action bar while armed, so the row list above it never
    /// changes length.
    pub confirm: Option<Confirm>,
}

/// Which way [`Draft::step_selected`] moves the value under the cursor.
/// A parameter rather than a second copy of the stepping match, so the
/// forward and backward paths cannot drift apart from each other.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StepDirection {
    Forward,
    Backward,
}

/// What one `space`/`shift+space` did to the row under the cursor.
///
/// Three answers rather than a `bool`, because [`Step::Inert`] and
/// [`Step::Refused`] are different things to say to a trader: the first
/// is "this row has no value that key changes", the second is "it does,
/// and that particular step would ask the config model for a state it
/// cannot hold". A key that appears inert is the defect class this
/// interaction model exists to remove, so both carry their own notice
/// rather than sharing one.
#[derive(Debug, Clone, PartialEq)]
pub enum Step {
    /// The value moved; the caller revalidates and commits.
    Changed,
    /// Nothing on this row has a value this key changes.
    Inert,
    /// Declined, with the reason to show. See [`Draft::step_selected`]'s
    /// `Destination::Doc` rule.
    Refused(String),
}

impl Step {
    /// Did the draft actually move? The one-bit question most callers —
    /// and most tests — are asking.
    pub fn changed(&self) -> bool {
        matches!(self, Step::Changed)
    }
}

impl Draft {
    /// The rows the edit stage paints, in order: every field, each
    /// ordered list's own items directly under it, then that list's
    /// available catalogue (§18.7.1).
    pub fn rows(&self) -> Vec<EditRow> {
        let mut out = Vec::new();
        for (i, field) in self.fields.iter().enumerate() {
            out.push(EditRow::Field(i));
            if let FieldKind::OrderedList { items, available } = &field.kind {
                for item in 0..items.len() {
                    out.push(EditRow::Item { field: i, item });
                }
                for item in 0..available.as_ref().map_or(0, Vec::len) {
                    out.push(EditRow::Available { field: i, item });
                }
            }
        }
        out
    }

    /// What the filter ranks a row by: exactly the text the row paints as
    /// its label — a field's label, the name of one of the object's own
    /// items, the name of an available one — and nothing more
    /// (the browse list's own rule, for the same reason: matching text
    /// the user cannot see breaks the agreement between what ranked and
    /// what is highlighted).
    pub fn row_label(&self, row: EditRow) -> String {
        match row {
            EditRow::Field(i) => self.fields[i].label.clone(),
            EditRow::Item { field, item } => match &self.fields[field].kind {
                FieldKind::OrderedList { items, .. } => items[item].name.clone(),
                _ => String::new(),
            },
            // An available row's label is its name too — the same text,
            // said by a different row, because it is the same column
            // seen from the other side of the verb that moves it.
            EditRow::Available { field, item } => match &self.fields[field].kind {
                FieldKind::OrderedList {
                    available: Some(available),
                    ..
                } => available[item].name.clone(),
                _ => String::new(),
            },
        }
    }

    /// The rows the edit stage shows: every row whose [`Draft::row_label`]
    /// [`crate::listfilter::rank`] matches the query, in ROW order —
    /// never score order. `Ranked::row` indexes [`Draft::rows`].
    ///
    /// This is where the edit stage's own filtering deliberately parts
    /// ways with the browse list's (review round 1): browse's rows are
    /// an unordered catalogue, so ranking by match quality is a pure
    /// improvement, but the edit stage's row order **is the data** — a
    /// view's column order, a grouping slot's chain order — and
    /// reordering it out from under a filter would be actively
    /// misleading rather than merely surprising. Two reasons, both still
    /// standing after §18.1 gave each block a header and each row of the
    /// object's own list a grip. First, the order is a value the trader
    /// is editing and
    /// `shift+j`/`shift+k` are how they edit it: neither can mean
    /// anything coherent against a list whose painted order `shift+j`
    /// does not control. Second, a section header marks where its block
    /// BEGINS — it says nothing about a row that a score sort has thrown
    /// into the middle of the wrong block, so under score order the
    /// header would be actively wrong rather than merely absent. So
    /// `rank` is used only to decide which rows survive the query;
    /// `sort_by_key` afterwards restores row order among the survivors,
    /// discarding nothing but the score-derived ordering.
    pub fn visible_rows(&self) -> Vec<crate::listfilter::Ranked> {
        let labels: Vec<String> = self.rows().into_iter().map(|r| self.row_label(r)).collect();
        let mut ranked = crate::listfilter::rank(&labels, &self.query);
        ranked.sort_by_key(|m| m.row);
        ranked
    }

    /// The row the cursor is on, if the cursor is in range — indexed
    /// through [`Draft::visible_rows`], so every verb acts on the row the
    /// trader is actually looking at, filtered or not (§18.3).
    pub fn selected_row(&self) -> Option<EditRow> {
        let rows = self.rows();
        self.visible_rows()
            .get(self.selected)
            .and_then(|m| rows.get(m.row).copied())
    }

    /// Re-point the cursor at `row`'s own position in the FILTERED list,
    /// after a verb has changed which row that is — the identity-based
    /// replacement for the arithmetic `self.selected = self.selected -
    /// item + end` used to do when `selected` indexed the unfiltered
    /// [`Draft::rows`] directly. Leaves `selected` where it is if `row`
    /// is no longer visible under the current query, which none of its
    /// callers can actually produce (moving an item never changes its
    /// own label), but is the honest fallback for a future
    /// one that might. Neither an *add* nor a *removal* calls this — the
    /// cursor stays behind on the row that was next rather than following
    /// the item from one list into the other (`step_selected`'s and
    /// `remove_selected`'s own comments have the ruling).
    fn follow(&mut self, row: EditRow) {
        let rows = self.rows();
        if let Some(position) = self
            .visible_rows()
            .iter()
            .position(|m| rows.get(m.row) == Some(&row))
        {
            self.selected = position;
        }
    }

    /// Has anything changed since the last applied edit? Compared against
    /// the baseline rather than tracked with a flag, so putting a value
    /// back where it started changes nothing and writes nothing.
    ///
    /// `true` in exactly three situations, and a reader relying on this
    /// invariant should count on no others:
    ///
    /// 1. *within* the keystroke that changed a field, before
    ///    [`apply::commit_edit`] records it and moves the baseline;
    /// 2. while a [`Confirm::Fork`] is waiting on its answer — the change
    ///    is on the draft and not yet recorded anywhere else;
    /// 3. indefinitely, for a change `commit_edit` **refused**: an error
    ///    diagnostic (`apply::blocking_diagnostic`) or a shell with no
    ///    writable user directory both return before `mark_saved()`, so
    ///    the value stays on the draft, painted and dirty, until a later
    ///    commit succeeds and carries it along. That is deliberate — the
    ///    keystroke is not lost — and it is why no verb here consults
    ///    dirtiness to decide whether something needs saving.
    ///
    /// `source` is compared too, not just `fields` — see
    /// [`Draft::baseline_source`]'s own doc for why a fields-only
    /// comparison is not enough once a verb (Scopes' `o`) can replace
    /// `source` out from under a painted summary.
    pub fn is_dirty(&self) -> bool {
        self.fields != self.baseline || self.source != self.baseline_source
    }

    /// Put every field, and `source`, back to the last applied state —
    /// what a declined [`Confirm::Fork`] leaves behind, so the screen
    /// never shows a value that is neither applied nor persisted.
    pub fn revert_to_baseline(&mut self) {
        self.fields = self.baseline.clone();
        self.source = self.baseline_source.clone();
    }

    /// The object's own items of the ordered-list field named `key` —
    /// what is written, counted and reorderable.
    pub fn list_items(&self, key: &str) -> Option<&[ListItem]> {
        self.fields
            .iter()
            .find(|f| f.key == key)
            .and_then(|f| match &f.kind {
                FieldKind::OrderedList { items, .. } => Some(items.as_slice()),
                _ => None,
            })
    }

    /// The catalogue of what may JOIN the ordered-list field named `key`
    /// — `None` where no catalogue exists at all (Groupings' `dimensions`,
    /// where ticking is membership; a key naming no ordered list), and
    /// `Some`, possibly empty, where one does. The two answers are
    /// different facts and callers act on them differently
    /// ([`FieldKind::OrderedList`]'s own doc), so this deliberately does
    /// not flatten them into one empty slice.
    pub fn available_items(&self, key: &str) -> Option<&[ListItem]> {
        self.fields
            .iter()
            .find(|f| f.key == key)
            .and_then(|f| match &f.kind {
                FieldKind::OrderedList { available, .. } => available.as_deref(),
                _ => None,
            })
    }

    /// The selected option of the `Choice` field named `key`.
    pub fn choice(&self, key: &str) -> Option<&str> {
        self.fields
            .iter()
            .find(|f| f.key == key)
            .and_then(|f| match &f.kind {
                FieldKind::Choice { options, selected } => {
                    options.get(*selected).map(String::as_str)
                }
                _ => None,
            })
    }

    /// `space`: change the value under the cursor forward, recording it
    /// in the draft. `false` when the row has no value `space` can
    /// change, which the caller turns into a notice — a key that appears
    /// inert is the defect class this interaction model exists to
    /// remove.
    pub fn toggle_selected(&mut self) -> Step {
        self.step_selected(StepDirection::Forward)
    }

    /// `shift+space`: change the value under the cursor backward — the
    /// reverse twin of [`Draft::toggle_selected`]. Both share
    /// [`Draft::step_selected`] rather than carrying two near-identical
    /// copies of the same match, which is the failure this codebase keeps
    /// hitting (a fix applied to one copy and not the other).
    pub fn toggle_selected_back(&mut self) -> Step {
        self.step_selected(StepDirection::Backward)
    }

    /// Shared body of [`Draft::toggle_selected`] and
    /// [`Draft::toggle_selected_back`]: change the value under the
    /// cursor one step in `direction`, recording it in the draft.
    /// [`Step::Inert`] when the row has no value to change, or when the
    /// step would be a no-op (a `Number` already at the end `direction`
    /// points toward).
    ///
    /// **One step is refused rather than inert: emptying a
    /// [`Destination::Doc`] list.** Absence of a user-layer key in a
    /// domain's own doc means *inherit the layer beneath*, so an object
    /// rendered empty there cannot be written as "empty" and cannot be
    /// written as an absence either (`apply::object_value` has the full
    /// statement of that asymmetry). `GroupingSlots::set` refuses an
    /// empty chain and `GroupingSlots::from_doc` warns "slot N is empty;
    /// ignored", so the state is not representable in the model at all —
    /// and a state the model cannot hold must not be reachable by
    /// keystroke. Unticking a slot's last dimension is therefore declined
    /// here, at the one place both `space` and `shift+space` pass
    /// through, with `render::refuse_step` naming the verb (`d`/`r`) that
    /// does what the trader meant.
    ///
    /// An overlay list (`Destination::Presentation` — Views' `columns`)
    /// has no such rule: hiding every column of a view is a perfectly
    /// representable personalisation, and an empty overlay rendering IS
    /// an absence.
    fn step_selected(&mut self, direction: StepDirection) -> Step {
        let Some(row) = self.selected_row() else {
            return Step::Inert;
        };
        match row {
            EditRow::Field(i) => match &mut self.fields[i].kind {
                // A bool has only two values, so either direction is the
                // same flip.
                FieldKind::Bool(b) => {
                    *b = !*b;
                    Step::Changed
                }
                // Steps and wraps in both directions, which is what
                // makes the option just behind the current one reachable
                // in one key rather than the long way round —
                // `settings_view::step`'s own forward behaviour, mirrored
                // for `shift+space`.
                FieldKind::Choice { options, selected } => {
                    if options.len() < 2 {
                        return Step::Inert;
                    }
                    *selected = match direction {
                        StepDirection::Forward => (*selected + 1) % options.len(),
                        StepDirection::Backward => (*selected + options.len() - 1) % options.len(),
                    };
                    Step::Changed
                }
                FieldKind::Number { value, min, max } => {
                    match direction {
                        StepDirection::Forward if *value >= *max => return Step::Inert,
                        StepDirection::Backward if *value <= *min => return Step::Inert,
                        StepDirection::Forward => *value = (*value + 1).clamp(*min, *max),
                        StepDirection::Backward => *value = (*value - 1).clamp(*min, *max),
                    }
                    Step::Changed
                }
                // See `FieldKind`: `Text` is `i`'s and `MultiChoice`
                // needs a per-option row, neither of which Views has.
                // The `OrderedList` header row itself has no value —
                // its items, on the rows below, do.
                FieldKind::Text(_)
                | FieldKind::MultiChoice { .. }
                | FieldKind::OrderedList { .. } => Step::Inert,
            },
            EditRow::Available { field, item } => {
                let FieldKind::OrderedList { items, available } = &mut self.fields[field].kind
                else {
                    return Step::Inert;
                };
                let Some(available) = available.as_mut() else {
                    return Step::Inert;
                };
                if item >= available.len() {
                    return Step::Inert;
                }
                // Adding: out of the catalogue, onto the END of the
                // object's own list, shown.
                let mut entry = available.remove(item);
                entry.included = true;
                items.push(entry);
                // The cursor does NOT follow the item into the object's
                // own list: a trader adding several columns wants it on
                // the next available row, where their eye already is
                // (user ruling 2026-09-11). The added item moved
                // *earlier* in row order and its label is unchanged,
                // so the rows ahead of the next visible one are the
                // same set, merely reordered — its visible index is
                // the old cursor plus one. When the added item was the
                // catalogue's last row there is no next, and the same
                // index now holds the row that preceded it (the
                // previous available column, or the object's own last
                // item when there is none left), which is where the
                // cursor stays rather than running off the end.
                let last = self.visible_rows().len().saturating_sub(1);
                self.selected = (self.selected + 1).min(last);
                Step::Changed
            }
            EditRow::Item { field, item } => {
                // Read off the field before the list is borrowed mutably:
                // the destination is what decides whether emptying this
                // list is representable at all (see this function's doc).
                let dest = self.fields[field].dest;
                let label = self.fields[field].label.clone();
                let FieldKind::OrderedList { items, .. } = &mut self.fields[field].kind else {
                    return Step::Inert;
                };
                let Some(included) = items.get(item).map(|entry| entry.included) else {
                    return Step::Inert;
                };
                if included
                    && dest == Destination::Doc
                    && items.iter().filter(|i| i.included).count() == 1
                {
                    return Step::Refused(format!("{label} must keep at least one entry"));
                }
                // Inclusion is binary too, so both directions flip it.
                items[item].included = !included;
                Step::Changed
            }
        }
    }

    /// `shift+j` / `shift+k`: move the item under the cursor past the next
    /// VISIBLE item in that direction within the object's own list
    /// (§18.3) — under a filter that is what reordering means, and the
    /// count of hidden rows jumped over is returned so the notice can say
    /// so. `None` at either end of that list, and on a row that is not
    /// one of its items.
    ///
    /// An [`EditRow::Available`] row is one of those: the catalogue is
    /// unordered by construction (§18.7.2), so there is no order there to
    /// change and a "move" would be painted and never written. It is
    /// declined here rather than represented — which is also why the
    /// object's own last item has nowhere further down to go, even with a
    /// catalogue painted below it.
    pub fn move_item(&mut self, delta: i32) -> Option<usize> {
        let (field, item) = match self.selected_row()? {
            EditRow::Item { field, item } => (field, item),
            // Said, rather than left to a wildcard: an available row's
            // index means a position in the CATALOGUE, so reading it as
            // one in `items` would reorder a different column entirely.
            EditRow::Available { .. } | EditRow::Field(_) => return None,
        };
        let visible: BTreeSet<usize> = self.visible_rows().iter().map(|m| m.row).collect();
        let rows = self.rows();
        let row_of = |i: usize| {
            rows.iter()
                .position(|r| *r == EditRow::Item { field, item: i })
        };
        let FieldKind::OrderedList { items, .. } = &mut self.fields[field].kind else {
            return None;
        };
        let mut target = item;
        let mut skipped = 0usize;
        loop {
            let next = target.checked_add_signed(delta as isize)?;
            if next >= items.len() {
                return None;
            }
            target = next;
            if row_of(target).is_some_and(|r| visible.contains(&r)) {
                break;
            }
            skipped += 1;
        }
        let entry = items.remove(item);
        items.insert(target, entry);
        self.follow(EditRow::Item {
            field,
            item: target,
        });
        Some(skipped)
    }

    /// `x`: take the item under the cursor out of the object (§18.2) —
    /// the definitional twin of `space`'s hide.
    ///
    /// Whether a list has a separate membership at all is whether it HAS
    /// a catalogue (§18.7.2), never whether that catalogue happens to be
    /// empty right now, and never a `dest`. A Views draft with every
    /// available column already added still has one — an empty one — so
    /// `x` still removes there; a Groupings `dimensions` list has none at
    /// all (ticking IS membership) and refuses. Deciding by emptiness
    /// (which is what scanning one flat list's flags amounted to, in the
    /// build before this one) made `x` go dead on that first case: the moment a
    /// trader added the view's last available column, the list looked
    /// catalogue-less and `x` started refusing a removal it had done a
    /// keystroke earlier.
    ///
    /// [`Step::Refused`] rather than [`Step::Inert`] for both declined
    /// cases, so the footer says why: on a catalogue-less list, `space` is
    /// the verb that already unticks; on an available row, `space` is the
    /// verb that adds it — `x` has nothing to remove from a row that is
    /// not there yet. Neither reason names `d`/`r` the way `space`'s own
    /// "must keep at least one entry" refusal does, so `render` routes
    /// these straight to the footer rather than through `refuse_step`.
    pub fn remove_selected(&mut self) -> Step {
        let (field, item) = match self.selected_row() {
            Some(EditRow::Item { field, item }) => (field, item),
            Some(EditRow::Available { .. }) => {
                return Step::Refused("not in the view — space adds it".to_string());
            }
            _ => return Step::Inert,
        };
        let FieldKind::OrderedList { items, available } = &mut self.fields[field].kind else {
            return Step::Inert;
        };
        let Some(available) = available.as_mut() else {
            return Step::Refused("space unticks here".to_string());
        };
        if item >= items.len() {
            return Step::Inert;
        }
        let mut entry = items.remove(item);
        entry.included = false;
        available.push(entry);
        let last = available.len() - 1;
        // The cursor does NOT follow the item to the end of the
        // catalogue, for the same reason `space`'s add leaves it behind
        // (user ruling 2026-09-11): a trader removing several columns
        // wants it on the row that was next. The removed item moved
        // *later* in row order with its label unchanged, so the visible
        // rows ahead of the next one lost exactly one — the next row now
        // sits at the old index and `selected` is already right. The one
        // exception is a removal with nothing visible after it: the item
        // lands at the end, which is where it already was, so the old
        // index would still be on it — step back to the previous row
        // instead, the way `dd` on a buffer's last line does.
        let moved = EditRow::Available { field, item: last };
        let rows = self.rows();
        let under_cursor = self
            .visible_rows()
            .get(self.selected)
            .and_then(|m| rows.get(m.row).copied());
        if under_cursor == Some(moved) {
            self.selected = self.selected.saturating_sub(1);
        }
        Step::Changed
    }

    /// Which files this draft's changes have to be written to, and which
    /// field keys sent them there — the grouping a flush turns into one
    /// `config_write::edit` call per destination, never a write per field.
    ///
    /// A clean field contributes nothing, which is what keeps a
    /// presentation-only edit out of `views.toml` entirely.
    ///
    /// An ordered list contributes to its own destination **and** to
    /// [`Destination::Doc`] when its item *names* change, because which
    /// entries an object HAS is definitional however the list is
    /// presented:
    /// adding a column the view did not have changes the view, not its
    /// presentation (spec §8.1). Reordering, hiding and resizing never
    /// reach that branch, which is the split this whole design exists for.
    pub fn writes_by_destination(&self) -> BTreeMap<Destination, Vec<String>> {
        let mut out: BTreeMap<Destination, Vec<String>> = BTreeMap::new();
        for (i, field) in self.fields.iter().enumerate() {
            let before = self.baseline.get(i);
            if before == Some(field) {
                continue;
            }
            out.entry(field.dest).or_default().push(field.key.clone());
            if field.dest != Destination::Doc && membership_changed(before, field) {
                out.entry(Destination::Doc)
                    .or_default()
                    .push(field.key.clone());
            }
        }
        // `source` underlies only `Destination::Doc` renderings
        // (`views::doc_table`, `scopes::to_table`) — nothing presentation-
        // destined ever reads it — so a `source` change no field noticed
        // (see `Draft::baseline_source`'s own doc) still has to reach the
        // batch as a `Doc` write, with no field key of its own to name.
        if self.source != self.baseline_source {
            out.entry(Destination::Doc).or_default();
        }
        out
    }

    /// Accept the draft as applied: the current fields become the
    /// baseline, so the next keystroke's difference is that keystroke's
    /// alone.
    ///
    /// Called when the keystroke is **recorded** — before the flush
    /// merges, applies or writes anything — because the batch carries the
    /// whole rendered object rather than a delta, so the baseline's only
    /// job is to keep the next keystroke's difference to itself. A failed
    /// write is the one path that moves the baseline backwards:
    /// `apply::revert_failed_write` rebuilds the draft from the reverted
    /// config, baseline and all.
    pub fn mark_saved(&mut self) {
        self.baseline = self.fields.clone();
        self.baseline_source = self.source.clone();
    }

    /// A draft for an object nothing defines yet. Both baselines are
    /// EMPTY, so every field reads as changed and `writes_by_destination`
    /// names every destination — which is why `apply::commit_create`
    /// builds its own single Doc edit rather than calling `edits_for`: a
    /// new view's untouched column list would otherwise also queue an
    /// empty presentation write.
    pub fn new_object(name: &str, fields: Vec<Field>, source: toml::Table) -> Draft {
        Draft {
            name: name.to_string(),
            is_new: true,
            fields,
            source,
            baseline: Vec::new(),
            baseline_source: toml::Table::new(),
            selected: 0,
            query: String::new(),
            diagnostics: Vec::new(),
            confirm: None,
        }
    }
}

/// Did an ordered list's membership — the set of the object's OWN item
/// names, ignoring order and ignoring the available catalogue entirely —
/// change between `before` and `field`? See
/// [`Draft::writes_by_destination`].
///
/// The catalogue is not part of the comparison because it is not part of
/// the object: taking `available`'s names in too would call an add "no
/// membership change" the moment the added name already sat in the
/// catalogue — which is every add there is, since `space` only ever
/// promotes a row already painted — and route it to the overlay
/// destination alone, silently never writing the definitional file.
fn membership_changed(before: Option<&Field>, field: &Field) -> bool {
    let names = |f: &Field| match &f.kind {
        FieldKind::OrderedList { items, .. } => Some(
            items
                .iter()
                .map(|i| i.name.clone())
                .collect::<BTreeSet<_>>(),
        ),
        _ => None,
    };
    match (before.and_then(names), names(field)) {
        (Some(a), Some(b)) => a != b,
        _ => false,
    }
}

impl Domain {
    /// The fields of `object`, derived fresh from `config` — never cached,
    /// for the same reason [`Domain::objects`] is not.
    ///
    /// A `match`, unlike [`Domain::objects`]: what an object's fields are
    /// IS the domain-specific part, and there is nothing shared here for
    /// an adapter to diverge from.
    pub fn fields(self, config: &Config, object: Option<&str>) -> Vec<Field> {
        match self {
            Domain::Views => views::fields(config, object),
            Domain::Groupings => groupings::fields(config, object),
            Domain::Scopes => scopes::fields(config, object),
        }
    }

    /// A fresh [`Draft`] of `object`, validated once so the edit stage
    /// opens showing whatever is already wrong with it.
    pub fn draft(self, config: &Config, object: &str) -> Draft {
        let fields = self.fields(config, Some(object));
        let source = config
            .doc(self.doc())
            .and_then(|doc| doc.value.get(object))
            .and_then(|value| value.as_table())
            .cloned()
            .unwrap_or_default();
        let mut draft = Draft {
            name: object.to_string(),
            // `draft()` sets `is_new: false` in its literal.
            is_new: false,
            baseline: fields.clone(),
            fields,
            baseline_source: source.clone(),
            source,
            selected: 0,
            query: String::new(),
            diagnostics: Vec::new(),
            confirm: None,
        };
        draft.diagnostics = self.validate(&draft, config);
        draft
    }

    /// The draft `n` opens after a name is committed (§18.2): the
    /// adapter's empty-object fields — `fields(config, None)`, which every
    /// adapter already answers — over an empty source, validated once.
    /// Scopes' caller replaces the result with the frame's scope before
    /// committing (`scopes::overwrite_with`); the empty fields are still
    /// what this returns, so the pure core never reads a `Frame`.
    pub fn new_draft(self, config: &Config, name: &str) -> Draft {
        let mut draft = Draft::new_object(name, self.fields(config, None), toml::Table::new());
        draft.diagnostics = self.validate(&draft, config);
        draft
    }

    /// The draft rendered as the item that would be written to `dest`.
    ///
    /// `toml_edit::Item`, not `toml_edit::Table`: every Views field
    /// renders a table, but a grouping slot's own value is a bare array
    /// (`groupings::to_table`'s doc comment has the full story), and a
    /// return type that could only ever be a table would have had no way
    /// to say so.
    pub fn to_table(self, draft: &Draft, dest: Destination) -> toml_edit::Item {
        match self {
            Domain::Views => views::to_table(draft, dest),
            Domain::Groupings => groupings::to_table(draft, dest),
            Domain::Scopes => scopes::to_table(draft, dest),
        }
    }

    /// Everything wrong with the draft as it stands (spec §7.2), run on
    /// every field change, synchronously, with no debounce — it is a
    /// parse of a few hundred bytes.
    pub fn validate(self, draft: &Draft, config: &Config) -> Vec<Diagnostic> {
        match self {
            Domain::Views => views::validate(draft, config),
            Domain::Groupings => groupings::validate(draft, config),
            Domain::Scopes => scopes::validate(draft, config),
        }
    }
}

/// Put `item` in `document` under `name`, replacing whatever was there.
///
/// The one spelling of what a flush does to a file, shared by the write
/// path and by [`object_text`]. Whole-value replacement, never a
/// key-by-key merge: every doc these dialogs edit is atomic at depth one
/// (`config::merge::atomic_depth`), so a stale `hidden` left behind by a
/// merge would be a key nothing in the UI could remove.
///
/// `toml_edit::Item`, not `toml_edit::Table`: every domain but Groupings
/// stores an object as a table (`[tree]` header), but a grouping slot's
/// value is a bare array (`3 = ["book", "lhu"]`), and `DocumentMut`'s own
/// indexing assignment renders either shape correctly — a `Table` gets
/// its header, a `Value` gets `name = value` inline — without this
/// function needing to know which one it was handed.
pub(super) fn set_object(document: &mut toml_edit::DocumentMut, name: &str, item: toml_edit::Item) {
    document[name] = item;
}

/// One object's item as the TOML text a write would produce.
///
/// Goes through a `DocumentMut` rather than `Table::to_string`, which is
/// not the same thing and quietly loses work: a bare table renders only
/// its own key-value pairs, so an array of tables (`[[tree.columns]]`)
/// and a sub-table (`[tree.width]`) both need the document's header path
/// to appear at all.
pub fn object_text(name: &str, item: toml_edit::Item) -> String {
    let mut document = toml_edit::DocumentMut::new();
    set_object(&mut document, name, item);
    document.to_string()
}

/// One `toml::Table` (what `Config` holds) as a `toml_edit::Table` (what
/// a write produces).
///
/// A hand-written conversion rather than a `to_string`/`parse` round trip
/// so it cannot depend on the serializer's own idea of key ordering, and
/// so an array of tables comes out as an array of tables — `[[x.columns]]`
/// on the way out, which parses back to exactly what went in.
pub(super) fn toml_table_to_edit(table: &toml::Table) -> toml_edit::Table {
    let mut out = toml_edit::Table::new();
    for (key, value) in table {
        out.insert(key, toml_value_to_item(value));
    }
    out
}

fn toml_value_to_item(value: &toml::Value) -> toml_edit::Item {
    match value {
        toml::Value::Table(t) => toml_edit::Item::Table(toml_table_to_edit(t)),
        // An array whose every element is a table renders as
        // `[[name]]` blocks, which is how these config docs are written
        // by hand and how they read back identically.
        toml::Value::Array(a) if !a.is_empty() && a.iter().all(|v| v.is_table()) => {
            let mut arr = toml_edit::ArrayOfTables::new();
            for element in a {
                if let Some(t) = element.as_table() {
                    arr.push(toml_table_to_edit(t));
                }
            }
            toml_edit::Item::ArrayOfTables(arr)
        }
        other => toml_edit::Item::Value(toml_value_to_value(other)),
    }
}

fn toml_value_to_value(value: &toml::Value) -> toml_edit::Value {
    match value {
        toml::Value::String(s) => s.as_str().into(),
        toml::Value::Integer(i) => (*i).into(),
        toml::Value::Float(f) => (*f).into(),
        toml::Value::Boolean(b) => (*b).into(),
        // No config doc these dialogs edit carries a datetime; kept
        // lossless anyway by going through toml_edit's own parser, and
        // degrading to the same text as a string rather than to a
        // silently wrong value if it ever cannot.
        toml::Value::Datetime(d) => match d.to_string().parse::<toml_edit::Datetime>() {
            Ok(dt) => dt.into(),
            Err(_) => d.to_string().into(),
        },
        toml::Value::Array(a) => {
            let mut arr = toml_edit::Array::new();
            for element in a {
                arr.push(toml_value_to_value(element));
            }
            arr.into()
        }
        toml::Value::Table(t) => {
            let mut inline = toml_edit::InlineTable::new();
            for (key, v) in t {
                inline.insert(key, toml_value_to_value(v));
            }
            inline.into()
        }
    }
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
    /// The object being edited. `None` in [`Stage::Browse`], and the only
    /// state this dialog stores rather than derives — deliberately, since
    /// it is also what the edit stage paints; see [`Draft`].
    pub draft: Option<Draft>,
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
            draft: None,
        }
    }

    /// The **pure half** of entering the edit stage — call
    /// [`render::enter_edit_stage`], never this, from anywhere with a
    /// `Window` in scope.
    ///
    /// It opens `object`'s edit stage, turning the `escape` ladder's
    /// `PreviousStage` rung on by constructing [`Stage::Edit`], and it
    /// sets the two things that decide where the next keystroke goes:
    ///
    /// - the **browse query is dropped**, because the edit stage has its
    ///   OWN filter (`Draft::query`, §18.3) — a separate cursor space
    ///   from the browse list's, since `Draft::selected` indexes the
    ///   draft's own `visible_rows()`, never `ObjectDialogState::
    ///   selected`'s list. A stale browse query left in `state.query`
    ///   would rank nothing here (the edit stage never reads it) while
    ///   still being live enough to eat `escape`'s `ClearQuery` rung the
    ///   moment the trader stepped back to browse — the newly-built
    ///   draft's own `query` is separately guaranteed empty here too,
    ///   defensively, though every constructor already starts it that
    ///   way;
    /// - the **mode goes back to `Normal`**, because `enter` opens an
    ///   object from filter mode too, and a stage left in `Filter` sends
    ///   the next `escape` down the `LeaveFilter` rung — which the edit
    ///   handler does not claim, so the shell's modal branch closes the
    ///   whole dialog instead of stepping back to the list, without ever
    ///   asking.
    ///
    /// Both are pure, and that is now the whole transition: the shared
    /// `Input` is emptied and blurred to match by `dialog::
    /// sync_dialog_text` (spec §16.1), which reads the mode and
    /// [`Self::effective_query`] after the handler returns rather than
    /// being hand-written beside each mutation. A mode and a focus that
    /// disagree is the one thing this dialog's "one switch" exists to
    /// prevent — the defect this method's own second half was once fixed
    /// for, and the reason that half has one owner now instead of a
    /// copy at every site. The blur still needs a `Window`, which this
    /// file may not name; what changed is that no caller has to remember
    /// it. This method stays visible only inside this module's subtree
    /// so [`render::enter_edit_stage`] remains the one door onto the
    /// stage change itself.
    pub(in crate::shell::objectdialog) fn enter_edit(&mut self, config: &Config, object: &str) {
        let mut draft = self.domain.draft(config, object);
        draft.query.clear();
        self.draft = Some(draft);
        self.stage = Stage::Edit {
            object: object.to_string(),
        };
        self.query.clear();
        self.mode = DialogMode::Normal;
        self.selected = 0;
        self.notice = None;
    }

    /// [`Self::enter_edit`]'s twin for a name `n` has just committed
    /// (§18.2): the same stage transition, over an already-built `draft`
    /// rather than one derived from `config`. A committed name has
    /// nothing in `config` to derive from yet — the write is still on its
    /// way through the debounced flush — and Scopes' caller has already
    /// replaced the draft's fields with the frame's current scope
    /// (`scopes::overwrite_with`), which a fresh `domain.draft(config,
    /// name)` call would throw away.
    ///
    /// Visible only inside this subtree for the same reason
    /// [`Self::enter_edit`] is: [`render::enter_edit_stage`] is the one
    /// door onto either, and a caller reaching this directly would skip
    /// the scroll reset and the frame request that door also owns.
    pub(in crate::shell::objectdialog) fn enter_edit_with(&mut self, mut draft: Draft) {
        // Same defensive clear `enter_edit` gives its own freshly-derived
        // draft — see that method's doc.
        draft.query.clear();
        self.stage = Stage::Edit {
            object: draft.name.clone(),
        };
        self.draft = Some(draft);
        self.query.clear();
        self.mode = DialogMode::Normal;
        self.selected = 0;
        self.notice = None;
    }

    /// Back to the browse list, dropping the draft. `selected` is left to
    /// the caller, which puts it back on the object just edited — by
    /// name, since the unfiltered list is a different list from the one
    /// the object was opened from.
    ///
    /// No mode is set here, and none needs to be: see
    /// [`render::leave_edit`], the one caller, for which modes can
    /// actually stand at this point and why the sync is left to settle
    /// the field and the focus from whichever one does.
    ///
    /// `query` is cleared here too, belt-and-braces: `set_query`'s
    /// one-way mirror (§18.3) should already have kept it empty for the
    /// whole life of the edit stage, but this is the same habit
    /// `cancel_naming` and `begin_naming` already keep of clearing it on
    /// every stage transition, rather than trusting an invariant a future
    /// change to `set_query` could quietly break.
    pub fn leave_edit(&mut self) {
        self.draft = None;
        self.stage = Stage::Browse;
        self.query.clear();
        self.notice = None;
    }

    /// Replace the query — the **write half** of the one-way mirror
    /// [`Self::effective_query`] reads back
    /// (the pure half of the `InputEvent::Change` subscription). Resets
    /// the selection to the top match (after an edit the old index
    /// points at an unrelated row) and drops the notice, which named a
    /// row the re-ranked list has just moved the selection off.
    ///
    /// The mirror is **one-way per stage**, never both at once: in
    /// [`Stage::Edit`] the shared `Input` is the edit stage's own filter
    /// (§18.3), so the keystroke belongs to `Draft::query`, and `state.
    /// query` — the browse list's cursor space — must not also change
    /// underneath it; everywhere else (browse, naming) it belongs to
    /// `state.query` as it always did. Writing both, as an earlier build
    /// of this method did, left a stale copy in whichever field the
    /// current stage was NOT reading: the edit stage's own `ClearQuery`
    /// rung only ever clears `Draft::query`, so a query typed while
    /// editing was still sitting in `state.query` after `escape` walked
    /// all the way back to browse — a filtered browse list under an
    /// empty-looking filter field, and a `leave_edit` cursor restore that
    /// silently failed to find the object it was looking for.
    pub fn set_query(&mut self, query: String) {
        if matches!(self.stage, Stage::Edit { .. })
            && let Some(draft) = self.draft.as_mut()
        {
            draft.query = query;
            draft.selected = 0;
        } else {
            self.query = query;
            self.selected = 0;
        }
        self.notice = None;
    }

    /// The query the open stage is filtering by — the draft's in
    /// `Stage::Edit`, the state's own otherwise (spec §16.2). The **read
    /// half** of [`Self::set_query`]'s one-way mirror: `dialog::
    /// sync_dialog_text` writes the shared `Input` from this, so a query
    /// left sitting in the other stage's slot can never reach the screen.
    pub fn effective_query(&self) -> &str {
        match (&self.stage, self.draft.as_ref()) {
            (Stage::Edit { .. }, Some(draft)) => draft.query.as_str(),
            _ => self.query.as_str(),
        }
    }

    /// `n`: the browse list stays, the filter row becomes the name field.
    /// The shared `Input` holds the name exactly as it holds a query —
    /// `set_query` mirrors it into `query` — so there is no second text
    /// buffer to keep in step.
    ///
    /// Pure, like every transition here: `DialogMode::Filter` is what
    /// gives the field the keys (through `dialogmode::focus_target`, via
    /// `dialog::sync_dialog_text`), and clearing `query` is what empties
    /// it — the sync writes the field from [`Self::effective_query`], so
    /// a browse filter left standing would be written straight back into
    /// the name field the trader is about to type into.
    pub fn begin_naming(&mut self) {
        self.stage = Stage::Naming;
        self.query.clear();
        self.mode = DialogMode::Filter;
        self.notice = None;
    }

    /// `escape` from [`Stage::Naming`]: back to browse, nothing written.
    /// Pure, and complete on its own — `Browse`, `Normal`, an empty
    /// `query` — so `dialog::sync_dialog_text` empties the field and
    /// hands the keys back to the shell root with nothing else to say.
    pub fn cancel_naming(&mut self) {
        self.stage = Stage::Browse;
        self.query.clear();
        self.mode = DialogMode::Normal;
        self.selected = 0;
        self.notice = None;
    }

    /// Whether `escape` has a stage to step back into before it closes
    /// the dialog — the third rung of
    /// [`crate::dialogmode::escape_step`]'s ladder, and this scaffold is
    /// its first consumer (the keybinding dialog is one flat list and
    /// always passes `false`).
    ///
    /// It is a predicate over the stage rather than a literal `false` at
    /// the call site, and that is what made the edit stage turn the rung
    /// on by merely existing: `enter_edit` constructs [`Stage::Edit`] and
    /// nothing in `render` had to remember to change. A `false` spelled
    /// at the call site is exactly the shape that gets left behind when a
    /// stage arrives, and the failure is silent — `escape` in the edit
    /// stage would close the whole dialog out from under the object being
    /// edited, instead of going back one stage.
    pub fn has_previous_stage(&self) -> bool {
        matches!(self.stage, Stage::Edit { .. } | Stage::Naming)
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
    use geode_core::config::{Config, ConfigSources, Layer, LayerDoc, Severity};

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
    fn groupings_always_lists_nine_slots_and_an_unconfigured_one_has_no_layer() {
        let config = config_from(&[(Layer::Desk, "groupings", "3 = [\"book\"]\n")]);
        let rows = Domain::Groupings.objects(&config);
        let names: Vec<&str> = rows.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, ["1", "2", "3", "4", "5", "6", "7", "8", "9"]);
        let three = &rows[2];
        assert_eq!(three.layer, Some(Layer::Desk));
        assert_eq!(three.summary, "book");
        let one = &rows[0];
        assert_eq!(one.layer, None, "nothing defines slot 1");
        assert_eq!(one.summary, "empty");
        assert!(!one.overridden);
    }

    #[test]
    fn views_have_no_roster_so_a_config_with_no_views_lists_nothing() {
        let config = config_from(&[]);
        assert!(Domain::Views.objects(&config).is_empty());
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
        assert_eq!(tree.layer, Some(Layer::User));
        assert!(
            tree.overridden,
            "user layer plus an earlier layer means overridden"
        );
        assert_eq!(wide.layer, Some(Layer::Builtin));
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

    /// A presentation-only override **is** an override. A desk view a
    /// trader has hidden a column on has a user-layer file naming it, so
    /// `r` has to be offered: telling them "no user override to revert"
    /// would be false about a file that demonstrably exists, and hiding a
    /// column is the commonest edit this whole design exists to make
    /// cheap. Spec §5.3 assumed presentation always accompanies a doc
    /// override; it does not — that is the entire point of §4.1's split.
    #[test]
    fn a_presentation_only_override_is_marked_overridden() {
        let config = config_from(&[
            (Layer::Desk, "views", "[tree]\ndataset = \"risk\"\n"),
            (
                Layer::User,
                "view_presentation",
                "[tree]\nhidden = [\"npv\"]\n",
            ),
        ]);
        let rows = Domain::Views.objects(&config);
        let tree = rows.iter().find(|r| r.name == "tree").expect("tree");
        assert_eq!(
            tree.layer,
            Some(Layer::Desk),
            "presentation forks nothing — the desk's copy still wins"
        );
        assert!(
            tree.overridden,
            "a user-layer view_presentation entry is a user override to revert"
        );
    }

    /// And the guard that keeps `r` non-destructive: reverting deletes the
    /// user's copy, so it may only be offered when something is left
    /// behind. A view only the USER layer defines, personalised on top, is
    /// still not overridden — reverting there would delete the view.
    #[test]
    fn a_user_only_object_with_presentation_is_still_not_overridden() {
        let config = config_from(&[
            (Layer::User, "views", "[mine]\ndataset = \"risk\"\n"),
            (
                Layer::User,
                "view_presentation",
                "[mine]\nhidden = [\"npv\"]\n",
            ),
        ]);
        let rows = Domain::Views.objects(&config);
        assert_eq!(rows.len(), 1);
        assert!(!rows[0].overridden);
    }

    /// §18.2's "a name any layer already holds is refused" spans the
    /// presentation overlay, not just the domain's own doc. A view the
    /// desk has since dropped can still be named by the trader's own
    /// `view_presentation.toml` (they hid a column on it while it
    /// existed); that name produces no browse row at all, so a create
    /// checking only the rows would happily make a fresh user view that
    /// silently inherits the orphaned overlay's `hidden`/`order`/`width`
    /// — and being user-only, `r` then refuses, leaving no dialog verb
    /// that can clear it.
    #[test]
    fn a_presentation_only_name_is_taken_even_with_no_row_to_show_for_it() {
        let config = config_from(&[(
            Layer::User,
            "view_presentation",
            "[tree]\nhidden = [\"npv\"]\n",
        )]);
        assert!(
            Domain::Views.objects(&config).is_empty(),
            "no layer of views.toml defines tree, so it has no browse row"
        );
        assert!(
            Domain::Views.name_taken(&config, "tree"),
            "the orphaned overlay still holds the name"
        );
        assert!(
            !Domain::Views.name_taken(&config, "fresh"),
            "and a name nothing holds is still free"
        );
    }

    /// The other two thirds of `name_taken`'s union, each on the domain
    /// that has it: a layered doc key (Views, any layer) and a roster
    /// name no doc defines (Groupings' nine slots).
    #[test]
    fn name_taken_spans_every_layers_doc_keys_and_the_roster() {
        let config = config_from(&[(Layer::Desk, "views", "[tree]\ndataset = \"risk\"\n")]);
        assert!(Domain::Views.name_taken(&config, "tree"));
        assert!(
            Domain::Groupings.name_taken(&config, "7"),
            "an unconfigured slot is still a name the roster holds"
        );
        assert!(!Domain::Scopes.name_taken(&config, "tree"));
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
        assert_eq!(tree.layer, Some(Layer::User));
        assert!(tree.overridden);
        assert!(
            tree.summary.contains('d'),
            "the summary must describe the copy that takes effect, got {:?}",
            tree.summary
        );
        let desk_only = rows.iter().find(|r| r.name == "desk_only").expect("desk");
        assert_eq!(desk_only.layer, Some(Layer::Desk));
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

    /// The rung the edit stage turns on. `Browse` has nothing behind
    /// it, so `escape` must reach the close rung; `Edit` does, and the
    /// ladder must stop there first.
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

    #[test]
    fn a_new_view_draft_picks_the_first_real_dataset_and_no_columns_of_its_own() {
        let config = config_from(&[(
            Layer::Builtin,
            "datasets",
            "[risk.columns.npv]\ntype = \"f64\"\nrole = \"measure\"\n",
        )]);
        let draft = Domain::Views.new_draft(&config, "mine");
        assert!(draft.is_new);
        assert_eq!(draft.name, "mine");
        assert_eq!(
            draft.choice("dataset"),
            Some("risk"),
            "not the empty placeholder"
        );
        // No columns are the view's OWN yet — `npv` is only in the
        // available catalogue, for `space` to add (§18.2).
        assert!(draft.list_items("columns").unwrap().is_empty());
        assert!(
            draft
                .available_items("columns")
                .unwrap()
                .iter()
                .all(|i| !i.included)
        );
        assert!(
            draft
                .diagnostics
                .iter()
                .all(|d| d.severity != Severity::Error),
            "{:?}",
            draft.diagnostics
        );
        // Everything counts as a change against an empty baseline.
        assert!(draft.is_dirty());
    }

    #[test]
    fn naming_is_a_stage_escape_can_step_back_from() {
        let mut state = ObjectDialogState::new(Domain::Views);
        assert!(!state.has_previous_stage());
        state.begin_naming();
        assert_eq!(state.stage, Stage::Naming);
        assert!(state.has_previous_stage());
        state.cancel_naming();
        assert_eq!(state.stage, Stage::Browse);
    }

    #[test]
    fn groupings_and_views_answer_a_new_draft_but_the_roster_domain_is_never_asked() {
        // Documented, not enforced: `n` is inert on Groupings in render.
        let config = config_from(&[]);
        assert!(Domain::Groupings.new_draft(&config, "4").is_new);
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

    /// `effective_query` is the read half of `set_query`'s one-way
    /// mirror (§16.2): each stage's own keystrokes land in — and are
    /// read back from — that stage's own slot, never the other one.
    #[test]
    fn the_effective_query_is_the_stages_own() {
        let config = config_from(&[(
            Layer::Desk,
            "views",
            "[tree]\ndataset = \"risk\"\n[[tree.columns]]\nname = \"npv\"\n",
        )]);
        let mut state = ObjectDialogState::new(Domain::Views);
        state.set_query("br".to_string());
        assert_eq!(state.effective_query(), "br");
        state.enter_edit(&config, "tree"); // pub(in objectdialog) — this test is inside the module tree
        assert_eq!(
            state.effective_query(),
            "",
            "entering the edit stage starts with no filter"
        );
        state.set_query("np".to_string());
        assert_eq!(state.effective_query(), "np");
        assert_eq!(
            state.query, "",
            "the browse query is untouched by an edit-stage keystroke"
        );
        state.leave_edit();
        assert_eq!(state.effective_query(), "");
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

    // ---- The edit stage ---------------------------------------------

    /// A fixture with a `datasets` doc (so the dataset `Choice` has real
    /// options) and a desk `views` doc — the shape the demo config has,
    /// which is what the edit stage is built against.
    fn demo_config() -> Config {
        config_from(&[
            (
                Layer::Builtin,
                "datasets",
                "[risk_snapshot.columns.book]\ntype = \"utf8\"\nrole = \"dimension\"\n\
                 [risk_snapshot.columns.npv]\ntype = \"f64\"\nrole = \"measure\"\n\
                 [risk_snapshot.columns.delta01]\ntype = \"f64\"\nrole = \"measure\"\n\
                 [other.columns.book]\ntype = \"utf8\"\nrole = \"dimension\"\n",
            ),
            (
                Layer::Desk,
                "views",
                "[tree]\ndataset = \"risk_snapshot\"\ngrouping = [\"book\"]\n\
                 [[tree.columns]]\nname = \"book\"\nkind = \"dimension\"\n\
                 [[tree.columns]]\nname = \"npv\"\n\
                 [[tree.columns]]\nname = \"delta01\"\n",
            ),
        ])
    }

    /// A draft of `name` with the cursor already on its first list item.
    /// `space` and `shift+j` are the column list's verbs, so that is where
    /// a trader who pressed `j` past the field rows would be — found by
    /// walking [`Draft::rows`] rather than hardcoded, so adding a field
    /// above the list does not silently move these tests onto a different
    /// row and keep passing.
    fn draft_for(name: &str) -> Draft {
        let config = demo_config();
        let mut draft = Domain::Views.draft(&config, name);
        draft.selected = draft
            .rows()
            .iter()
            .position(|r| matches!(r, EditRow::Item { .. }))
            .expect("the fixture view has columns");
        draft
    }

    fn list_items(draft: &Draft) -> &[ListItem] {
        draft
            .list_items("columns")
            .expect("the columns field is an ordered list")
    }

    fn list_names(draft: &Draft) -> Vec<String> {
        list_items(draft).iter().map(|i| i.name.clone()).collect()
    }

    /// A draft with one `Doc` field and one `Presentation` field changed.
    fn dirty_draft_touching_both() -> Draft {
        let mut draft = draft_for("tree");
        draft.toggle_selected(); // a column's inclusion — Presentation
        draft.selected = 0;
        draft.toggle_selected(); // the dataset — Doc
        draft
    }

    /// The whole reason Views is built first. A dragged width, a hidden
    /// column and a reordered list must never reach `views.toml`: writing
    /// them there forks the desk's view, and a forked view is frozen — the
    /// desk adds a column next week and the trader never sees it.
    #[test]
    fn presentation_fields_and_doc_fields_go_to_different_destinations() {
        let config = demo_config();
        let fields = Domain::Views.fields(&config, Some("tree"));
        let dataset = fields.iter().find(|f| f.key == "dataset").expect("dataset");
        assert_eq!(
            dataset.dest,
            Destination::Doc,
            "the dataset defines the view"
        );
        let columns = fields.iter().find(|f| f.key == "columns").expect("columns");
        assert_eq!(
            columns.dest,
            Destination::Presentation,
            "order, inclusion and width are presentation — a dragged width must \
             never fork a desk view"
        );
    }

    /// `space` toggles inclusion; `shift+j` moves the item, not the cursor.
    #[test]
    fn an_ordered_list_toggles_and_reorders() {
        let mut draft = draft_for("tree");
        let before: Vec<String> = list_names(&draft);
        draft.toggle_selected(); // space on item 0
        assert!(!list_items(&draft)[0].included);
        draft.move_item(1); // shift+j
        assert_eq!(
            list_names(&draft)[1],
            before[0],
            "the item moved, not the cursor"
        );
    }

    /// A draft with a single field of `kind`, cursor already on it —
    /// mirrors `Domain::draft`'s own construction so a directly-built
    /// field behaves exactly as one that came from a real config would.
    fn single_field_draft(kind: FieldKind) -> Draft {
        let field = Field {
            key: "value".to_string(),
            label: "Value".to_string(),
            kind,
            dest: Destination::Doc,
        };
        Draft {
            name: "test".to_string(),
            is_new: false,
            baseline: vec![field.clone()],
            fields: vec![field],
            source: toml::Table::new(),
            baseline_source: toml::Table::new(),
            selected: 0,
            query: String::new(),
            diagnostics: Vec::new(),
            confirm: None,
        }
    }

    /// `Number` refused at `max` and had no way down at all, so the first
    /// adapter with a real `Number` field would inherit one that can be
    /// raised and never lowered. (Groupings turned out not to be that
    /// adapter after all — its `slot` is display-only, see
    /// `groupings.rs`'s module doc — so this still guards a field no
    /// domain has built yet.)
    #[test]
    fn a_number_steps_both_ways_and_stops_at_each_end() {
        // At min: stepping back is a no-op returning false; forward moves it.
        let mut draft = single_field_draft(FieldKind::Number {
            value: 0,
            min: 0,
            max: 2,
        });
        assert!(
            !draft.toggle_selected_back().changed(),
            "already at min, so backward changes nothing"
        );
        assert_eq!(
            draft.fields[0].kind,
            FieldKind::Number {
                value: 0,
                min: 0,
                max: 2
            }
        );
        assert!(draft.toggle_selected().changed());
        assert_eq!(
            draft.fields[0].kind,
            FieldKind::Number {
                value: 1,
                min: 0,
                max: 2
            }
        );

        // At max: forward is a no-op; backward moves it.
        let mut draft = single_field_draft(FieldKind::Number {
            value: 2,
            min: 0,
            max: 2,
        });
        assert!(
            !draft.toggle_selected().changed(),
            "already at max, so forward changes nothing"
        );
        assert_eq!(
            draft.fields[0].kind,
            FieldKind::Number {
                value: 2,
                min: 0,
                max: 2
            }
        );
        assert!(draft.toggle_selected_back().changed());
        assert_eq!(
            draft.fields[0].kind,
            FieldKind::Number {
                value: 1,
                min: 0,
                max: 2
            }
        );
    }

    /// `Choice` wraps forward; it must wrap backward symmetrically, or the
    /// last option is three keystrokes away and the first is unreachable
    /// from it.
    #[test]
    fn a_choice_wraps_in_both_directions() {
        let options = vec!["a".to_string(), "b".to_string(), "c".to_string()];
        let mut draft = single_field_draft(FieldKind::Choice {
            options: options.clone(),
            selected: 0,
        });
        assert!(draft.toggle_selected_back().changed());
        assert_eq!(
            draft.choice("value"),
            Some("c"),
            "from 0, back wraps to last"
        );

        let mut draft = single_field_draft(FieldKind::Choice {
            options,
            selected: 2,
        });
        assert!(draft.toggle_selected().changed());
        assert_eq!(
            draft.choice("value"),
            Some("a"),
            "from last, forward wraps to 0"
        );
    }

    /// A draft groups its writes by destination, so one save is at most one
    /// `config_write::edit` per file — never a write per field.
    #[test]
    fn a_save_groups_writes_by_destination() {
        let draft = dirty_draft_touching_both();
        let groups = draft.writes_by_destination();
        assert_eq!(groups.len(), 2);
        assert!(groups.contains_key(&Destination::Doc));
        assert!(groups.contains_key(&Destination::Presentation));
    }

    /// The `Doc` write is a whole-object override, so anything the field
    /// vocabulary does not model has to survive it. A view rendered from
    /// `dataset` plus a list of names would silently drop the grouping,
    /// the sort, the column kinds and every format — and the user would
    /// only find out when the blotter came back wrong.
    #[test]
    fn a_doc_write_preserves_everything_the_fields_do_not_model() {
        let config = config_from(&[(
            Layer::Desk,
            "views",
            "[tree]\ndataset = \"risk_snapshot\"\ngrouping = [\"book\"]\n\
             [[tree.columns]]\nname = \"book\"\nkind = \"dimension\"\n\
             [[tree.columns]]\nname = \"npv\"\nlabel = \"NPV\"\n\
             [tree.columns.format]\nprecision = 3\n\
             [[tree.sort]]\ncolumn = \"npv\"\ndescending = true\n",
        )]);
        let draft = Domain::Views.draft(&config, "tree");
        let text = object_text("tree", Domain::Views.to_table(&draft, Destination::Doc));
        for kept in [
            "grouping",
            "book",
            "kind = \"dimension\"",
            "label = \"NPV\"",
            "precision = 3",
            "descending = true",
        ] {
            assert!(text.contains(kept), "{kept:?} was dropped from:\n{text}");
        }
    }

    /// And the mirror image, which is the fork this whole design exists to
    /// prevent: nothing presentational may appear in the view's own table.
    #[test]
    fn a_doc_write_carries_no_order_no_hidden_and_no_width() {
        let mut draft = draft_for("tree");
        draft.toggle_selected(); // hide the first column
        draft.move_item(1); // and reorder it
        let text = object_text("tree", Domain::Views.to_table(&draft, Destination::Doc));
        for presentational in ["hidden", "order", "width"] {
            assert!(
                !text.contains(presentational),
                "{presentational:?} reached views.toml, forking the desk's view:\n{text}"
            );
        }
        // The column order in the view's own table is the source's, not
        // the trader's: order is presentation.
        assert_eq!(
            text.matches("name = ").count(),
            3,
            "all three columns are still there:\n{text}"
        );
        assert!(
            text.find("name = \"book\"").unwrap() < text.find("name = \"npv\"").unwrap(),
            "the source's column order is untouched by a reorder:\n{text}"
        );
    }

    /// The presentation table is what `view_presentation.toml` receives:
    /// `order` once the trader has actually reordered, and `hidden` and
    /// `width` only when there is something to say.
    #[test]
    fn the_presentation_table_holds_order_hidden_and_width() {
        let mut draft = draft_for("tree");
        if let Some(FieldKind::OrderedList { items, .. }) = draft
            .fields
            .iter_mut()
            .find(|f| f.key == "columns")
            .map(|f| &mut f.kind)
        {
            items[1].width = Some(120.0);
        }
        draft.toggle_selected(); // hide `book`
        draft.move_item(1); // and move it below `npv`
        let text = object_text(
            "tree",
            Domain::Views.to_table(&draft, Destination::Presentation),
        );
        assert!(
            text.contains("order = [\"npv\", \"book\", \"delta01\"]"),
            "{text}"
        );
        assert!(text.contains("hidden = [\"book\"]"), "{text}");
        assert!(text.contains("npv = 120.0"), "{text}");
    }

    /// **A presentation save must not freeze the desk's own values.**
    /// `order` and `width` are both read off the EFFECTIVE view, which
    /// already carries whatever `views.toml` declared — so writing every
    /// column's position and every declared width back into the trader's
    /// file pins the desk's layout for that trader against the desk's
    /// later changes. That is the same freeze [`Destination`] exists to
    /// prevent, one field-granularity down, and it fires on the single
    /// commonest edit there is: hiding one column.
    #[test]
    fn a_presentation_save_writes_only_what_the_trader_changed() {
        let config = config_from(&[(
            Layer::Desk,
            "views",
            "[tree]\ndataset = \"risk_snapshot\"\n\
             [[tree.columns]]\nname = \"book\"\n\
             [[tree.columns]]\nname = \"npv\"\nwidth = 140\n",
        )]);
        let mut draft = Domain::Views.draft(&config, "tree");
        draft.selected = draft
            .rows()
            .iter()
            .position(|r| matches!(r, EditRow::Item { .. }))
            .expect("the fixture view has columns");
        assert_eq!(
            draft.list_items("columns").map(|i| i[1].width),
            Some(Some(140.0)),
            "the desk's width reaches the draft — that is why it can be copied back"
        );

        draft.toggle_selected(); // hide `book`, and change nothing else
        let text = object_text(
            "tree",
            Domain::Views.to_table(&draft, Destination::Presentation),
        );
        assert!(text.contains("hidden = [\"book\"]"), "{text}");
        assert!(
            !text.contains("order"),
            "nothing was reordered, so pinning the desk's order is a freeze:\n{text}"
        );
        assert!(
            !text.contains("140"),
            "the desk's own width must not be copied into the trader's file:\n{text}"
        );
    }

    /// A view with nothing hidden and no widths writes neither key, rather
    /// than empty containers — and because the table is replaced whole, a
    /// previously hidden column really does come back.
    #[test]
    fn unhiding_the_last_column_removes_the_hidden_key_entirely() {
        let mut draft = draft_for("tree");
        draft.toggle_selected();
        draft.toggle_selected();
        let text = object_text(
            "tree",
            Domain::Views.to_table(&draft, Destination::Presentation),
        );
        assert!(!text.contains("hidden"), "{text}");
        assert!(!text.contains("width"), "{text}");
    }

    /// A width already in `view_presentation.toml` survives an edit that
    /// was about something else. Losing it on save would be the same class
    /// of defect as forking the view: silent, and only visible next time
    /// the trader looked at the column.
    #[test]
    fn an_existing_width_survives_a_save_that_was_about_something_else() {
        let config = config_from(&[
            (
                Layer::Desk,
                "views",
                "[tree]\ndataset = \"risk_snapshot\"\n\
                 [[tree.columns]]\nname = \"book\"\n[[tree.columns]]\nname = \"npv\"\n",
            ),
            (
                Layer::User,
                "view_presentation",
                "[tree]\nhidden = [\"book\"]\n[tree.width]\nnpv = 140.0\n",
            ),
        ]);
        let mut draft = Domain::Views.draft(&config, "tree");
        assert_eq!(
            draft.list_items("columns").map(|i| i[1].width),
            Some(Some(140.0)),
            "the draft has to read the width back before it can keep it"
        );
        // Unhide `book`, which is a change to something else entirely.
        draft.selected = draft
            .rows()
            .iter()
            .position(|r| matches!(r, EditRow::Item { .. }))
            .unwrap();
        draft.toggle_selected();
        let text = object_text(
            "tree",
            Domain::Views.to_table(&draft, Destination::Presentation),
        );
        assert!(
            text.contains("npv = 140.0"),
            "the width was dropped:\n{text}"
        );
        assert!(!text.contains("hidden"), "{text}");
    }

    /// Membership is definitional however the list is presented: a column
    /// the object gained or lost has to reach `views.toml` even though the
    /// field it lives on is a `Presentation` field. Reordering and hiding
    /// must not — that is the split.
    #[test]
    fn changing_the_column_set_writes_the_doc_but_reordering_does_not() {
        let mut reordered = draft_for("tree");
        reordered.move_item(1);
        assert_eq!(
            reordered
                .writes_by_destination()
                .keys()
                .copied()
                .collect::<Vec<_>>(),
            vec![Destination::Presentation],
            "a reorder is presentation and nothing else"
        );

        let mut dropped = draft_for("tree");
        if let Some(FieldKind::OrderedList { items, .. }) = dropped
            .fields
            .iter_mut()
            .find(|f| f.key == "columns")
            .map(|f| &mut f.kind)
        {
            items.remove(0);
        }
        let groups = dropped.writes_by_destination();
        assert!(
            groups.contains_key(&Destination::Doc),
            "losing a column changes what the view IS: {groups:?}"
        );
        let text = object_text("tree", Domain::Views.to_table(&dropped, Destination::Doc));
        assert!(
            !text.contains("name = \"book\""),
            "and the view's own table has to lose it:\n{text}"
        );
    }

    /// `fields` takes `Option<&str>` because spec §4 has it serve the
    /// create path too, and a name nothing defines has to come back as an
    /// object with no items of its OWN rather than a panic — a
    /// `Choice` with no dataset selected and an empty column list. Its
    /// catalogue is not empty though (§18.2): with no view to read,
    /// `current` falls back to the schema's first dataset
    /// (alphabetically, `other`, which has one column, `book`), and that
    /// column is in the catalogue for `space` to add.
    #[test]
    fn an_object_that_does_not_exist_has_empty_fields_rather_than_panicking() {
        let config = demo_config();
        for object in [None, Some("nonesuch")] {
            let fields = Domain::Views.fields(&config, object);
            assert_eq!(fields.len(), 2, "{object:?}");
            let FieldKind::OrderedList { items, available } = &fields[1].kind else {
                panic!("{object:?} columns field must be an ordered list");
            };
            assert!(
                items.is_empty(),
                "{object:?} should have no columns of its own yet, got {items:?}"
            );
            assert_eq!(
                available
                    .as_deref()
                    .unwrap_or_default()
                    .iter()
                    .map(|i| i.name.as_str())
                    .collect::<Vec<_>>(),
                ["book"],
                "only the dataset's available ones, got {available:?}"
            );
            // The destinations do not depend on the object, which is what
            // lets the create path reuse them unchanged.
            assert_eq!(fields[0].dest, Destination::Doc);
            assert_eq!(fields[1].dest, Destination::Presentation);
        }
    }
    /// A draft nobody touched writes nothing at all — which is what keeps
    /// opening a view, looking at it and pressing `s` from forking it.
    #[test]
    fn an_untouched_draft_is_clean_and_writes_nothing() {
        let draft = draft_for("tree");
        assert!(!draft.is_dirty());
        assert!(draft.writes_by_destination().is_empty());
    }

    /// Putting a value back where it started makes the draft clean again:
    /// dirtiness is a comparison against the baseline, not a flag that
    /// latches on the first keystroke.
    #[test]
    fn a_change_undone_leaves_the_draft_clean() {
        let mut draft = draft_for("tree");
        draft.toggle_selected();
        assert!(draft.is_dirty());
        draft.toggle_selected();
        assert!(!draft.is_dirty(), "the value is back where it started");
    }

    /// `space` on a row with nothing to change reports `false` so the
    /// dialog can say so — a key that appears inert is the defect class
    /// the interaction model exists to remove.
    #[test]
    fn space_on_a_row_with_no_value_says_it_did_nothing() {
        let mut draft = draft_for("tree");
        draft.selected = draft
            .rows()
            .iter()
            .position(|r| *r == EditRow::Field(1))
            .expect("the columns field's own header row");
        assert!(
            !draft.toggle_selected().changed(),
            "a list header has no value"
        );
        assert!(!draft.is_dirty());
    }

    /// `shift+k` at the top and `shift+j` at the bottom do nothing and say
    /// so, rather than wrapping the item round the list.
    #[test]
    fn an_item_does_not_move_past_either_end() {
        let mut draft = draft_for("tree");
        let before = list_names(&draft);
        assert!(
            draft.move_item(-1).is_none(),
            "the first item has nowhere up to go"
        );
        draft.selected += 2;
        assert!(draft.move_item(1).is_none(), "nor the last one down");
        assert_eq!(list_names(&draft), before);
    }

    /// Validation runs against the DRAFT alone (spec §7.2). Validating the
    /// merged doc instead would report every other broken view in the
    /// config against the one object the user is editing.
    #[test]
    fn validation_sees_the_draft_and_not_the_rest_of_the_config() {
        let config = config_from(&[
            (
                Layer::Builtin,
                "datasets",
                "[risk_snapshot.columns.npv]\ntype = \"f64\"\nrole = \"measure\"\n",
            ),
            (
                Layer::Desk,
                "views",
                "[good]\ndataset = \"risk_snapshot\"\n[[good.columns]]\nname = \"npv\"\n\
                 [broken]\n[[broken.columns]]\nname = \"npv\"\n",
            ),
        ]);
        let good = Domain::Views.draft(&config, "good");
        assert!(
            good.diagnostics.is_empty(),
            "another view's problem is not this view's: {:?}",
            good.diagnostics
        );
        let broken = Domain::Views.draft(&config, "broken");
        assert!(
            broken
                .diagnostics
                .iter()
                .any(|d| d.message.contains("dataset")),
            "the object being edited IS validated: {:?}",
            broken.diagnostics
        );
    }

    /// A dataset the schema does not have is the one problem the doc's own
    /// reader cannot see — it parses cleanly and then compiles to nothing.
    #[test]
    fn a_dataset_the_schema_lacks_is_reported() {
        let config = config_from(&[
            (
                Layer::Builtin,
                "datasets",
                "[risk_snapshot.columns.npv]\ntype = \"f64\"\nrole = \"measure\"\n",
            ),
            (
                Layer::Desk,
                "views",
                "[tree]\ndataset = \"retired\"\n[[tree.columns]]\nname = \"npv\"\n",
            ),
        ]);
        let draft = Domain::Views.draft(&config, "tree");
        assert!(
            draft
                .diagnostics
                .iter()
                .any(|d| d.message.contains("retired")),
            "{:?}",
            draft.diagnostics
        );
        // And the choice can still show it, or stepping the field would
        // silently change the view the moment it was touched.
        assert_eq!(draft.choice("dataset"), Some("retired"));
    }

    /// Each destination knows exactly one file. `Presentation` is one of
    /// the two scaffold doors onto `view_presentation.toml` — the other is
    /// `Domain::presentation_doc`, asserted below — and both read the name
    /// off the Views adapter's own constant, which is what keeps them from
    /// naming different files.
    #[test]
    fn each_destination_names_its_own_file() {
        assert_eq!(Destination::Doc.doc(Domain::Views), "views");
        assert_eq!(
            Destination::Presentation.doc(Domain::Views),
            "view_presentation"
        );
        // The second door onto the same name, and the two domains that
        // have no overlay file at all.
        assert_eq!(Domain::Views.presentation_doc(), Some("view_presentation"));
        assert_eq!(Domain::Groupings.presentation_doc(), None);
        assert_eq!(Domain::Scopes.presentation_doc(), None);
    }

    /// Saving accepts the draft so `s` pressed twice does not write twice.
    #[test]
    fn marking_a_draft_saved_makes_it_clean_again() {
        let mut draft = draft_for("tree");
        draft.toggle_selected();
        assert!(draft.is_dirty());
        draft.mark_saved();
        assert!(!draft.is_dirty());
        assert!(draft.writes_by_destination().is_empty());
    }

    /// Entering the edit stage turns the ladder's `PreviousStage` rung on
    /// and drops the browse query with it: `state.query` (browse) and
    /// `Draft::query` (edit, §18.3) are two separate cursor spaces, so a
    /// stale browse query left applied would rank a list the trader is no
    /// longer looking at, and would also eat the `escape` that was meant
    /// to go back a stage before the ladder ever reached the edit stage's
    /// own query.
    #[test]
    fn entering_the_edit_stage_arms_the_previous_stage_rung() {
        let config = demo_config();
        let mut state = ObjectDialogState::new(Domain::Views);
        state.set_query("tr".to_string());
        assert!(!state.has_previous_stage());
        state.mode = DialogMode::Filter;
        state.enter_edit(&config, "tree");
        assert!(state.has_previous_stage());
        assert_eq!(state.query, "", "the browse query does not follow you in");
        assert_eq!(
            state.mode,
            DialogMode::Normal,
            "nor does filter mode — escape would take the LeaveFilter rung \
             and close the dialog instead of going back a stage"
        );
        assert_eq!(
            state.draft.as_ref().map(|d| d.name.clone()),
            Some("tree".to_string())
        );
        state.leave_edit();
        assert!(!state.has_previous_stage());
        assert!(state.draft.is_none());
    }

    /// The confirm prompts name the object rather than asking "are you
    /// sure". They no longer spell out the fork's long-term cost ("it stops
    /// following the desk") — a user ruling of 2026-09-11 (commit 9916049)
    /// took that sentence out of every prompt; the cost is documented in
    /// spec §4.1, and the prompt's job is to name what is being copied.
    #[test]
    fn a_confirm_names_the_object_and_the_consequence() {
        assert!(Confirm::Delete.prompt("tree").contains("tree"));
        assert!(Confirm::Revert.prompt("tree").contains("tree"));
        // The fork prompt names the object and the act; it deliberately
        // does not spell out "it stops following the desk" (user ruling
        // 2026-09-11: that clause read as a warning about the act rather
        // than a description of it).
        let fork = Confirm::Fork.prompt("tree");
        assert!(fork.contains("tree"), "{fork}");
        assert!(fork.starts_with("Copy"), "{fork}");
        assert!(!fork.contains("desk"), "{fork}");
    }

    /// `Confirm::Overwrite`'s two prompts must each be true of the case
    /// they describe (Part 2a Task 5 review round 1, the Major): a
    /// user-owned scope really does lose its previous contents, but a
    /// desk/builtin-owned one does not — the desk's copy is still there,
    /// `r`-revertible — so it must say so instead of claiming a loss.
    /// Neither prompt spells out "it stops following the desk" any more
    /// (user ruling 2026-09-11); the forked case discloses its
    /// reversibility through `'r' reverts` alone.
    #[test]
    fn overwrite_prompts_tell_the_truth_about_what_it_costs() {
        let owned = Confirm::Overwrite { forks: false }.prompt("mine");
        assert!(owned.contains("mine"), "{owned}");
        assert!(owned.contains("lost"), "{owned}");
        assert!(
            !owned.contains("reverts"),
            "a user-owned scope's prompt must not claim a fork: {owned}"
        );

        let forked = Confirm::Overwrite { forks: true }.prompt("mine");
        assert!(forked.contains("mine"), "{forked}");
        assert!(
            !forked.contains("desk"),
            "the 'stops following the desk' clause was retired: {forked}"
        );
        assert!(
            forked.contains("reverts"),
            "a desk-owned scope's prompt must say r reverts: {forked}"
        );
        assert!(
            !forked.contains("lost"),
            "nothing is lost when the desk's own copy is still there: {forked}"
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

    // ---- Task 4, as §18.7 left it: the object's own list and the
    // ---- catalogue behind it -----------------------------------------

    /// A Groupings list has no available catalogue at ALL (§18.7.1) —
    /// ticking IS membership there — so `remove_selected` refuses and
    /// names the verb that does work here (`space`) rather than silently
    /// unticking. Decided by the catalogue's absence, never by a `dest`
    /// and never by whether some catalogue happens to be empty right now.
    #[test]
    fn a_groupings_list_has_no_available_block_and_x_refuses() {
        let config = config_from(&[
            (
                Layer::Builtin,
                "datasets",
                "[risk.columns.book]\ntype = \"utf8\"\nrole = \"dimension\"\n",
            ),
            (Layer::User, "groupings", "3 = [\"book\"]\n"),
        ]);
        let mut draft = Domain::Groupings.draft(&config, "3");
        assert!(
            draft.available_items("dimensions").is_none(),
            "no catalogue exists here, which is not the same as an empty one"
        );
        draft.selected = 2;
        assert_eq!(
            draft.remove_selected(),
            Step::Refused("space unticks here".to_string())
        );
    }

    /// **The regression this ruling exists to close.** A Views draft
    /// whose catalogue is EMPTY — every column the dataset offers already
    /// added — must still let `x` remove one: a catalogue that exists and
    /// holds nothing is not the same as no catalogue at all (§18.7.2).
    /// Deciding by emptiness (as `dest`-scanning and item-scanning builds
    /// before it both did, each in its own way) has `x` go dead the moment
    /// the trader finishes adding everything the dataset offers.
    #[test]
    fn remove_selected_still_works_when_the_catalogue_is_empty() {
        let config = config_from(&[
            (
                Layer::Builtin,
                "datasets",
                "[risk.columns.npv]\ntype = \"f64\"\nrole = \"measure\"\ngrain = \"position\"\n",
            ),
            (
                Layer::Desk,
                "views",
                "[tree]\ndataset = \"risk\"\n[[tree.columns]]\nname = \"npv\"\n",
            ),
        ]);
        let mut draft = Domain::Views.draft(&config, "tree");
        // `npv` is the dataset's only column and the view already has it,
        // so the catalogue exists and is empty.
        assert!(
            draft
                .available_items("columns")
                .is_some_and(<[_]>::is_empty),
            "sanity: this fixture's catalogue must exist and be empty"
        );
        draft.selected = 2; // the one item row: npv
        assert!(draft.remove_selected().changed());
        assert!(draft.list_items("columns").unwrap().is_empty());
        assert_eq!(
            draft
                .available_items("columns")
                .unwrap()
                .iter()
                .map(|i| i.name.as_str())
                .collect::<Vec<_>>(),
            ["npv"],
            "and the removed column is what the catalogue now holds"
        );
    }

    // ---- Task 6: filtering the edit stage (§18.3) --------------------

    /// The edit stage's own filter narrows [`Draft::visible_rows`], and
    /// [`Draft::selected`] indexes that filtered list — the same
    /// convention the browse stage already has — so a verb like `space`
    /// acts on the row the trader is looking at, not on whatever
    /// unfiltered row happened to sit at that position.
    #[test]
    fn the_edit_stage_filters_by_label_and_the_cursor_indexes_the_filtered_list() {
        let config = config_from(&[
            (
                Layer::Builtin,
                "datasets",
                "[risk.columns.book]\ntype = \"utf8\"\nrole = \"dimension\"\n\
                 [risk.columns.lhu]\ntype = \"utf8\"\nrole = \"dimension\"\n",
            ),
            (Layer::User, "groupings", "3 = [\"book\", \"lhu\"]\n"),
        ]);
        let mut draft = Domain::Groupings.draft(&config, "3");
        draft.query = "lhu".to_string();
        let visible = draft.visible_rows();
        assert_eq!(visible.len(), 1);
        draft.selected = 0;
        assert_eq!(
            draft.selected_row(),
            Some(EditRow::Item { field: 1, item: 1 })
        );
        assert!(
            draft.toggle_selected().changed(),
            "the verb acts on the filtered row"
        );
    }

    /// `shift+j`/`shift+k` under a filter move the item past the next
    /// VISIBLE neighbour, not the next literal one — jumping over
    /// whatever the query is hiding — and report how many rows it
    /// skipped so the caller's notice can say so. Single-letter column
    /// names cannot carry this test (a one-character candidate cannot
    /// hold a query long enough to pick two of three of them out from
    /// the third — see `listfilter::rank`'s subsequence matcher), so the
    /// fixture uses longer names and a query ("al") that is a subsequence
    /// of `alpha` and of `charlie` but not of `bravo`.
    #[test]
    fn reordering_under_a_filter_moves_past_the_hidden_rows_and_says_how_many() {
        let config = config_from(&[
            (
                Layer::Builtin,
                "datasets",
                "[risk.columns.alpha]\ntype = \"utf8\"\nrole = \"dimension\"\n\
                 [risk.columns.bravo]\ntype = \"utf8\"\nrole = \"dimension\"\n\
                 [risk.columns.charlie]\ntype = \"utf8\"\nrole = \"dimension\"\n",
            ),
            (
                Layer::User,
                "groupings",
                "3 = [\"alpha\", \"bravo\", \"charlie\"]\n",
            ),
        ]);
        let mut draft = Domain::Groupings.draft(&config, "3");
        draft.query = "al".to_string(); // hides bravo, the middle item
        assert_eq!(
            draft.visible_rows().len(),
            2,
            "sanity: the query must hide exactly bravo"
        );
        draft.selected = 0; // alpha
        assert_eq!(draft.move_item(1), Some(1), "one hidden row skipped");
        let names: Vec<&str> = draft
            .list_items("dimensions")
            .unwrap()
            .iter()
            .map(|i| i.name.as_str())
            .collect();
        assert_eq!(&names[..3], ["bravo", "charlie", "alpha"]);
        // Row order, not fuzzy score, decides the filtered index now
        // (review round 1): after the move, `rows()` reads
        // [.., charlie (row 3), alpha (row 4)], and since "al" still
        // scores alpha far higher than charlie, a score-ordered
        // `visible_rows` would have put alpha BACK at index 0 — the
        // identity check below would pass either way, which is exactly
        // why this index is asserted explicitly too.
        assert_eq!(
            draft.selected, 1,
            "alpha is the second VISIBLE row in row order, not the first by score"
        );
        assert_eq!(
            draft.selected_row(),
            Some(EditRow::Item { field: 1, item: 2 }),
            "the cursor followed alpha"
        );
    }

    /// Review round 1: the edit stage's row order IS the data — column
    /// order, chain order — so a filter must narrow it, never reorder
    /// it. `visible_rows` used to sort by fuzzy score like the browse
    /// list does, which put a later, better-scoring match ahead of an
    /// earlier, worse-scoring one; that made `shift+j` unable to change
    /// the painted order at all, and left §18.1's section header sitting
    /// above whichever row happened to score best rather than at the
    /// start of its block. `apple` (row 3) scores far higher against "a"
    /// than `banana` (row 2) does (an idx-0 match earns a head-start bonus —
    /// see `palette::fuzzy_match_lowered`), so a score-ordered list would
    /// paint them in the wrong order despite both matching.
    #[test]
    fn visible_rows_lists_matches_in_row_order_not_score_order() {
        let config = config_from(&[
            (
                Layer::Builtin,
                "datasets",
                "[risk.columns.banana]\ntype = \"utf8\"\nrole = \"dimension\"\n\
                 [risk.columns.apple]\ntype = \"utf8\"\nrole = \"dimension\"\n",
            ),
            (Layer::User, "groupings", "3 = [\"banana\", \"apple\"]\n"),
        ]);
        let mut draft = Domain::Groupings.draft(&config, "3");
        draft.query = "a".to_string();
        let rows = draft.rows();
        let names: Vec<String> = draft
            .visible_rows()
            .iter()
            .filter_map(|m| rows.get(m.row))
            .map(|r| draft.row_label(*r))
            .collect();
        assert_eq!(
            names,
            vec!["banana".to_string(), "apple".to_string()],
            "list order must win over apple's higher fuzzy score"
        );
    }
}
